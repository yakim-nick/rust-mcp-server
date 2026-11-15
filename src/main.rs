use std::process::ExitCode;

use rmcp::ServiceExt;
use rmcp::transport::stdio;
use rust_mcp_server::{ProjectServer, Sandbox};

const USAGE: &str = "rust-mcp-server [PROJECT_DIR]\n\n\
Serves three read-only tools (list_files, read_file, git_log) over MCP on stdio.\n\
PROJECT_DIR defaults to the current directory; nothing outside it is ever read.";

#[tokio::main]
async fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let root = match args.next().as_deref() {
        Some("--help" | "-h") => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some("--version" | "-V") => {
            println!("rust-mcp-server {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some(path) => path.to_string(),
        None => ".".to_string(),
    };

    let sandbox = match Sandbox::new(&root) {
        Ok(sandbox) => sandbox,
        Err(e) => {
            eprintln!("error: cannot use {root:?} as the project directory: {e}");
            return ExitCode::from(2);
        }
    };
    // stdout carries the protocol, so diagnostics go to stderr only.
    eprintln!("rust-mcp-server: serving {}", sandbox.root().display());

    let service = match ProjectServer::new(sandbox).serve(stdio()).await {
        Ok(service) => service,
        Err(e) => {
            eprintln!("error: failed to start the MCP service: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = service.waiting().await {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
