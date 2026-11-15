//! End-to-end tests. The server runs in-process on one end of an in-memory pipe and the
//! SDK's own client talks to it on the other, so the real MCP protocol is exercised.
//! Two more tests spawn the compiled binary to check the stdio transport.

use std::fs;
use std::path::Path;
use std::process::Command;

use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::service::RunningService;
use rmcp::{ClientHandler, RoleClient, ServiceExt};
use rust_mcp_server::{ProjectServer, Sandbox};
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

#[derive(Debug, Clone, Default)]
struct TestClient;

impl ClientHandler for TestClient {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::default()
    }
}

type Client = RunningService<RoleClient, TestClient>;

async fn connect(root: &Path) -> Client {
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let server = ProjectServer::new(Sandbox::new(root).unwrap());
    tokio::spawn(async move {
        let running = server.serve(server_io).await.unwrap();
        let _ = running.waiting().await;
    });
    TestClient.serve(client_io).await.unwrap()
}

/// Call a tool; returns (is_error, text) of the first content block.
async fn call(client: &Client, tool: &'static str, args: Value) -> (bool, String) {
    let params = CallToolRequestParams::new(tool)
        .with_arguments(args.as_object().cloned().unwrap_or_default());
    let result = client
        .call_tool(params)
        .await
        .expect("protocol-level success");
    let json = serde_json::to_value(&result).unwrap();
    let is_error = json["isError"].as_bool().unwrap_or(false);
    let text = json["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    (is_error, text)
}

fn project() -> TempDir {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src/nested")).unwrap();
    fs::write(dir.path().join("README.md"), "# Demo\nhello\n").unwrap();
    fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(dir.path().join("src/nested/deep.txt"), "deep").unwrap();
    fs::write(dir.path().join(".env"), "TOKEN=super-secret").unwrap();
    fs::create_dir_all(dir.path().join(".git")).unwrap();
    fs::write(dir.path().join(".git/config"), "[core]").unwrap();
    dir
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=Tester",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .status()
        .expect("git is installed");
    assert!(status.success(), "git {args:?} failed");
}

#[tokio::test]
async fn advertises_exactly_the_three_read_only_tools_with_schemas() {
    let dir = project();
    let client = connect(dir.path()).await;
    let tools = client.list_all_tools().await.unwrap();
    let mut names: Vec<_> = tools.iter().map(|t| t.name.to_string()).collect();
    names.sort();
    assert_eq!(names, ["git_log", "list_files", "read_file"]);
    for tool in &tools {
        assert!(
            tool.description
                .as_deref()
                .is_some_and(|d| d.contains("Read-only")),
            "{}",
            tool.name
        );
        assert_eq!(tool.input_schema.get("type"), Some(&json!("object")));
    }
    let read = tools.iter().find(|t| t.name == "read_file").unwrap();
    assert_eq!(read.input_schema["required"], json!(["path"]));
}

#[tokio::test]
async fn list_files_shows_a_sorted_tree_and_hides_secrets() {
    let dir = project();
    let client = connect(dir.path()).await;
    let (is_error, text) = call(&client, "list_files", json!({})).await;
    assert!(!is_error);
    // The default depth is 2 levels, so deep.txt (a third level) is not shown yet.
    assert_eq!(
        text,
        ".\nsrc/\n  nested/\n  main.rs (13 bytes)\nREADME.md (13 bytes)\n"
    );
    assert!(!text.contains(".env") && !text.contains(".git"), "{text}");

    let (_, deeper) = call(&client, "list_files", json!({ "max_depth": 3 })).await;
    assert!(deeper.contains("    deep.txt (4 bytes)"), "{deeper}");
}

#[tokio::test]
async fn list_files_respects_depth_and_subdirectories() {
    let dir = project();
    let client = connect(dir.path()).await;
    let (_, shallow) = call(&client, "list_files", json!({ "max_depth": 1 })).await;
    assert!(
        shallow.contains("src/\n") && !shallow.contains("main.rs"),
        "{shallow}"
    );
    let (_, sub) = call(&client, "list_files", json!({ "path": "src" })).await;
    assert!(sub.starts_with("src\n") && sub.contains("main.rs"), "{sub}");
    let (is_error, text) = call(&client, "list_files", json!({ "path": "README.md" })).await;
    assert!(is_error && text.contains("not a directory"), "{text}");
}

#[tokio::test]
async fn list_files_stops_at_the_entry_limit_and_says_so() {
    let dir = tempdir().unwrap();
    for i in 0..600 {
        fs::write(dir.path().join(format!("f{i:04}.txt")), "x").unwrap();
    }
    let client = connect(dir.path()).await;
    let (is_error, text) = call(&client, "list_files", json!({})).await;
    assert!(!is_error);
    assert_eq!(
        text.lines().filter(|l| l.ends_with("(1 bytes)")).count(),
        500
    );
    assert!(text.contains("stopped after 500 entries"), "{text}");
}

#[cfg(unix)]
#[tokio::test]
async fn list_files_skips_symlinks() {
    let dir = project();
    let outside = tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
    let client = connect(dir.path()).await;
    let (_, text) = call(&client, "list_files", json!({})).await;
    assert!(!text.contains("escape"), "{text}");
}

#[tokio::test]
async fn read_file_returns_the_content() {
    let dir = project();
    let client = connect(dir.path()).await;
    let (is_error, text) = call(&client, "read_file", json!({ "path": "src/main.rs" })).await;
    assert!(!is_error);
    assert_eq!(text, "fn main() {}\n");
}

#[tokio::test]
async fn read_file_truncates_long_files_and_never_splits_a_character() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("big.txt"), "€".repeat(100)).unwrap(); // 300 bytes
    let client = connect(dir.path()).await;
    let (is_error, text) = call(
        &client,
        "read_file",
        json!({ "path": "big.txt", "max_bytes": 10 }),
    )
    .await;
    assert!(!is_error, "{text}");
    assert!(
        text.starts_with("€€€\n\n[truncated at 10 bytes]"),
        "{text:?}"
    );
}

#[tokio::test]
async fn read_file_refuses_secrets_traversal_directories_binaries_and_missing_files() {
    let dir = project();
    fs::write(dir.path().join("image.bin"), [0xff, 0xfe, 0x00, 0x01]).unwrap();
    let client = connect(dir.path()).await;
    for (path, expected) in [
        (".env", "not allowed"),
        (".git/config", "not allowed"),
        ("../etc/passwd", "must be relative"),
        ("/etc/passwd", "must be relative"),
        ("src/../../x", "must be relative"),
        ("src", "not a regular file"),
        ("image.bin", "not valid UTF-8"),
        ("missing.txt", "not found"),
    ] {
        let (is_error, text) = call(&client, "read_file", json!({ "path": path })).await;
        assert!(is_error, "{path} must be an error");
        assert!(text.contains(expected), "{path}: {text}");
        assert!(!text.contains("super-secret"), "secret leaked via {path}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn read_file_cannot_follow_a_symlink_out_of_the_project() {
    let dir = project();
    let outside = tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "outside-secret").unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("secret.txt"),
        dir.path().join("link.txt"),
    )
    .unwrap();
    std::os::unix::fs::symlink(dir.path().join(".env"), dir.path().join("env-link.txt")).unwrap();
    let client = connect(dir.path()).await;
    for path in ["link.txt", "env-link.txt"] {
        let (is_error, text) = call(&client, "read_file", json!({ "path": path })).await;
        assert!(is_error, "{path}");
        assert!(!text.contains("secret"), "{text}");
    }
}

#[tokio::test]
async fn git_log_lists_commits_newest_first_and_respects_the_limit() {
    let dir = project();
    git(dir.path(), &["init", "-q"]);
    fs::write(dir.path().join("a.txt"), "1").unwrap();
    git(dir.path(), &["add", "a.txt"]);
    git(dir.path(), &["commit", "-q", "-m", "first commit"]);
    fs::write(dir.path().join("a.txt"), "2").unwrap();
    git(dir.path(), &["commit", "-q", "-am", "second commit"]);

    let client = connect(dir.path()).await;
    let (is_error, text) = call(&client, "git_log", json!({})).await;
    assert!(!is_error, "{text}");
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].ends_with("Tester  second commit"), "{}", lines[0]);
    assert!(lines[1].ends_with("Tester  first commit"), "{}", lines[1]);

    let (_, one) = call(&client, "git_log", json!({ "limit": 1 })).await;
    assert_eq!(one.lines().count(), 1);
    let (_, clamped) = call(&client, "git_log", json!({ "limit": 0 })).await;
    assert_eq!(clamped.lines().count(), 1, "limit is clamped to at least 1");
}

#[tokio::test]
async fn git_log_outside_a_repository_is_a_tool_error_not_a_crash() {
    let dir = tempdir().unwrap();
    let client = connect(dir.path()).await;
    let (is_error, text) = call(&client, "git_log", json!({})).await;
    assert!(is_error);
    assert!(text.contains("git log failed"), "{text}");
}

#[tokio::test]
async fn bad_arguments_are_readable_tool_errors_and_unknown_tools_are_protocol_errors() {
    let dir = project();
    let client = connect(dir.path()).await;
    // Invalid arguments come back as `isError: true` text, so a model can fix its own call.
    let (is_error, text) = call(&client, "read_file", json!({})).await;
    assert!(is_error && text.contains("missing field `path`"), "{text}");
    let (is_error, text) = call(&client, "read_file", json!({ "path": 5 })).await;
    assert!(is_error && text.contains("expected a string"), "{text}");
    // A tool that does not exist is a JSON-RPC error (the call itself is invalid).
    assert!(
        client
            .call_tool(CallToolRequestParams::new("delete_everything"))
            .await
            .is_err()
    );
}

#[test]
fn the_binary_speaks_mcp_over_stdio_and_keeps_stdout_clean() {
    use std::io::Write;
    use std::process::Stdio;

    let dir = project();
    let mut child = Command::new(env!("CARGO_BIN_EXE_rust-mcp-server"))
        .arg(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let requests = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"read_file","arguments":{"path":"README.md"}}}"#,
    ];
    {
        let stdin = child.stdin.as_mut().unwrap();
        for line in requests {
            writeln!(stdin, "{line}").unwrap();
        }
    }
    drop(child.stdin.take()); // closing stdin ends the session
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    let messages: Vec<Value> = stdout
        .lines()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|_| panic!("stdout must only carry JSON-RPC, got: {line}"))
        })
        .collect();
    assert_eq!(messages.len(), 2, "one reply per request: {stdout}");
    let init = messages.iter().find(|m| m["id"] == 1).unwrap();
    assert_eq!(init["result"]["serverInfo"]["name"], "rust-mcp-server");
    let read = messages.iter().find(|m| m["id"] == 2).unwrap();
    assert_eq!(read["result"]["content"][0]["text"], "# Demo\nhello\n");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("serving"),
        "diagnostics go to stderr"
    );
}

#[test]
fn the_binary_refuses_a_bad_project_directory() {
    let output = Command::new(env!("CARGO_BIN_EXE_rust-mcp-server"))
        .arg("/definitely/not/here")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "nothing on stdout when startup fails"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot use"));
}
