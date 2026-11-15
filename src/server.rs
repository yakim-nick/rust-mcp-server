//! The MCP server: three read-only tools over a sandboxed project directory.

use std::fmt::Write as _;
use std::fs;
use std::io::Read;
use std::process::Command;
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;

use crate::sandbox::{Sandbox, is_denied_name};

/// Most entries `list_files` returns before it stops and says so.
pub const MAX_LIST_ENTRIES: usize = 500;
/// Deepest directory level `list_files` will descend.
pub const MAX_DEPTH: usize = 5;
pub const DEFAULT_DEPTH: usize = 2;
pub const DEFAULT_READ_BYTES: usize = 64 * 1024;
pub const MAX_READ_BYTES: usize = 256 * 1024;
pub const DEFAULT_LOG_LIMIT: usize = 10;
pub const MAX_LOG_LIMIT: usize = 50;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListFilesArgs {
    /// Directory to list, relative to the project root. Defaults to the root.
    pub path: Option<String>,
    /// How many directory levels to descend (1-5, default 2).
    pub max_depth: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadFileArgs {
    /// File to read, relative to the project root.
    pub path: String,
    /// Maximum bytes to return (default 65536, at most 262144). Longer files are truncated.
    pub max_bytes: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GitLogArgs {
    /// Number of commits to show (1-50, default 10).
    pub limit: Option<usize>,
}

#[derive(Clone)]
pub struct ProjectServer {
    sandbox: Arc<Sandbox>,
    tool_router: ToolRouter<Self>,
}

fn ok(text: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(text)])
}

/// A failure the model can read and react to (`isError: true`), not a protocol error.
fn fail(text: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(text)])
}

#[tool_router]
impl ProjectServer {
    pub fn new(sandbox: Sandbox) -> Self {
        Self {
            sandbox: Arc::new(sandbox),
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "List files and directories under a project directory as an indented tree. \
                       Read-only. Hides .git and .env files; symlinks are skipped."
    )]
    pub async fn list_files(&self, Parameters(args): Parameters<ListFilesArgs>) -> CallToolResult {
        let depth = args.max_depth.unwrap_or(DEFAULT_DEPTH).clamp(1, MAX_DEPTH);
        let dir = match self.sandbox.resolve(args.path.as_deref().unwrap_or("")) {
            Ok(dir) => dir,
            Err(e) => return fail(e.to_string()),
        };
        if !dir.is_dir() {
            return fail("path is not a directory");
        }
        let mut out = String::new();
        let mut count = 0;
        let truncated = walk(&dir, depth, 0, &mut out, &mut count);
        if truncated {
            let _ = writeln!(
                out,
                "... stopped after {MAX_LIST_ENTRIES} entries, use a deeper path or a smaller max_depth"
            );
        }
        if count == 0 {
            out.push_str("(empty)\n");
        }
        ok(format!("{}\n{out}", self.sandbox.display_relative(&dir)))
    }

    #[tool(
        description = "Read a UTF-8 text file from the project. Read-only. Large files are truncated; \
                       binary files, .git and .env files are refused."
    )]
    pub async fn read_file(&self, Parameters(args): Parameters<ReadFileArgs>) -> CallToolResult {
        let limit = args
            .max_bytes
            .unwrap_or(DEFAULT_READ_BYTES)
            .clamp(1, MAX_READ_BYTES);
        let path = match self.sandbox.resolve(&args.path) {
            Ok(path) => path,
            Err(e) => return fail(e.to_string()),
        };
        if !path.is_file() {
            return fail("path is not a regular file");
        }
        let file = match fs::File::open(&path) {
            Ok(file) => file,
            Err(e) => return fail(format!("could not open file: {}", e.kind())),
        };
        // Read one byte more than the limit to learn whether the file was truncated.
        let mut buffer = Vec::with_capacity(limit + 1);
        if let Err(e) = file.take(limit as u64 + 1).read_to_end(&mut buffer) {
            return fail(format!("could not read file: {}", e.kind()));
        }
        let truncated = buffer.len() > limit;
        buffer.truncate(limit);
        let text = match decode_utf8_prefix(&buffer, truncated) {
            Some(text) => text,
            None => return fail("file is not valid UTF-8 text (binary files are not supported)"),
        };
        if truncated {
            ok(format!("{text}\n\n[truncated at {limit} bytes]"))
        } else {
            ok(text)
        }
    }

    #[tool(
        description = "Show recent git commits of the project (hash, author, date, subject), newest first. \
                       Read-only. Fails if the project is not a git repository."
    )]
    pub async fn git_log(&self, Parameters(args): Parameters<GitLogArgs>) -> CallToolResult {
        let limit = args
            .limit
            .unwrap_or(DEFAULT_LOG_LIMIT)
            .clamp(1, MAX_LOG_LIMIT);
        // Fixed arguments, no shell: nothing from the request is interpolated into a command line
        // except the validated number.
        let output = Command::new("git")
            .arg("--no-pager")
            .arg("-C")
            .arg(self.sandbox.root())
            .args([
                "log",
                "-n",
                &limit.to_string(),
                "--date=short",
                "--pretty=format:%h  %ad  %an  %s",
            ])
            .output();
        match output {
            Ok(out) if out.status.success() => {
                let text = String::from_utf8_lossy(&out.stdout).into_owned();
                if text.trim().is_empty() {
                    ok("(no commits yet)")
                } else {
                    ok(text)
                }
            }
            Ok(out) => fail(format!(
                "git log failed (is this a git repository with commits?): {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )),
            Err(e) => fail(format!("could not run git: {}", e.kind())),
        }
    }
}

#[tool_handler(
    router = self.tool_router,
    name = "rust-mcp-server",
    version = "0.1.0", // must equal Cargo.toml (checked by a test); the macro only accepts literals
    instructions = "Read-only access to one project directory: list_files shows the tree, \
                    read_file returns a text file, git_log shows recent commits. \
                    Paths are relative to the project root; .git and .env files are hidden."
)]
impl ServerHandler for ProjectServer {}

/// Depth-first directory walk writing an indented tree. Returns true when it stopped at
/// [`MAX_LIST_ENTRIES`]. Entries are sorted (directories first) so output is stable.
fn walk(
    dir: &std::path::Path,
    max_depth: usize,
    level: usize,
    out: &mut String,
    count: &mut usize,
) -> bool {
    let Ok(read) = fs::read_dir(dir) else {
        return false;
    };
    let mut entries: Vec<_> = read.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| (!e.file_type().is_ok_and(|t| t.is_dir()), e.file_name()));

    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if is_denied_name(&name) || file_type.is_symlink() {
            continue;
        }
        if *count >= MAX_LIST_ENTRIES {
            return true;
        }
        *count += 1;
        let indent = "  ".repeat(level);
        if file_type.is_dir() {
            let _ = writeln!(out, "{indent}{name}/");
            if level + 1 < max_depth && walk(&entry.path(), max_depth, level + 1, out, count) {
                return true;
            }
        } else {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            let _ = writeln!(out, "{indent}{name} ({size} bytes)");
        }
    }
    false
}

/// Decode bytes as UTF-8. When the buffer was cut at a byte limit it may end inside a
/// multi-byte character, so an incomplete trailing sequence is dropped instead of failing.
fn decode_utf8_prefix(bytes: &[u8], truncated: bool) -> Option<String> {
    match std::str::from_utf8(bytes) {
        Ok(text) => Some(text.to_string()),
        Err(e) if truncated && e.error_len().is_none() => {
            Some(String::from_utf8_lossy(&bytes[..e.valid_up_to()]).into_owned())
        }
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertised_version_matches_cargo_toml() {
        let info = ProjectServer::new(Sandbox::new(".").unwrap()).get_info();
        assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(info.server_info.name, "rust-mcp-server");
    }

    #[test]
    fn utf8_prefix_drops_a_split_trailing_character_only_when_truncated() {
        let euro = "€".as_bytes(); // 3 bytes
        assert_eq!(decode_utf8_prefix(&euro[..2], true).as_deref(), Some(""));
        assert_eq!(
            decode_utf8_prefix(&euro[..2], false),
            None,
            "not truncated: genuinely invalid"
        );
        assert_eq!(decode_utf8_prefix(b"ok", false).as_deref(), Some("ok"));
        assert_eq!(
            decode_utf8_prefix(&[0xff, 0xfe], true),
            None,
            "invalid bytes are never accepted"
        );
    }
}
