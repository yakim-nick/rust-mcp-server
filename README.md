# rust-mcp-server

[![CI](https://github.com/yakim-nick/rust-mcp-server/actions/workflows/ci.yml/badge.svg)](https://github.com/yakim-nick/rust-mcp-server/actions/workflows/ci.yml)

A small **read-only [MCP](https://modelcontextprotocol.io) server** written in Rust.
It lets an AI assistant (or any MCP client) look around **one project directory**:
see the file tree, read text files and read the recent git history. Nothing outside
that directory is ever opened, and nothing can be written or executed through it.

Built with the official Rust SDK, [`rmcp`](https://crates.io/crates/rmcp) 3.10, over
the stdio transport. It is a learning project and a template for small tool servers.

## Tools

| Tool | Arguments | What it does |
|------|-----------|--------------|
| `list_files` | `path?`, `max_depth?` (1-5, default 2) | Indented tree of a directory, sorted, directories first. Stops at 500 entries and says so. |
| `read_file` | `path`, `max_bytes?` (default 64 KiB, max 256 KiB) | Returns a UTF-8 text file; longer files are truncated and marked. Binary files are refused. |
| `git_log` | `limit?` (1-50, default 10) | `hash  date  author  subject`, newest first. Fails cleanly outside a git repository. |

Failures (bad path, missing argument, not a repo) are returned as **tool errors**
(`isError: true` with a readable message), so a model can correct its own call.
Only an unknown tool name is a JSON-RPC protocol error.

## Install and connect

Needs Rust 1.89+ (set by a dependency) and `git` on the `PATH` for `git_log`.

```bash
git clone https://github.com/yakim-nick/rust-mcp-server && cd rust-mcp-server
cargo build --release                  # binary: target/release/rust-mcp-server (about 4.5 MB)
./target/release/rust-mcp-server /path/to/project
```

The server speaks MCP on stdin/stdout, so you normally let a client start it. For a
client that uses the common `mcpServers` JSON config:

```json
{
  "mcpServers": {
    "project": {
      "command": "/absolute/path/to/rust-mcp-server",
      "args": ["/absolute/path/to/project"]
    }
  }
}
```

For Claude Code the equivalent command is
`claude mcp add project -- /absolute/path/to/rust-mcp-server /absolute/path/to/project`.
I have **not** tested it inside Claude Code or another desktop client; it was tested with
the SDK's own client and with raw JSON-RPC against the real binary (see Tests).

## Security model

This server hands file contents to a language model, so the boundary matters.

- **Confinement.** Every path is resolved in `src/sandbox.rs`: it must be relative, may not
  contain `..`, and is canonicalised (symlinks resolved) and must still be inside the
  project root. A symlink pointing outside the project cannot be followed. Listings skip
  symlinks entirely.
- **Deny list.** `.git` (anything inside it) and files named `.env` or `.env.*` are never
  listed or readable, including through a symlink inside the project.
- **Limits.** Reads are capped at 256 KiB, listings at 500 entries and depth 5.
- **No shell.** `git_log` runs `git` with fixed arguments; the only request value used is a
  clamped number.
- **Read-only.** There is no tool that writes, deletes or runs anything.

What it does **not** protect against:

- The deny list is deliberately small. A secret in another file (`id_rsa`,
  `credentials.json`, `config/secrets.yml`) **is readable** if it is inside the project.
  Point the server at a directory that contains nothing you would not show the model.
- `git_log` runs the real `git` binary against that directory, and git honours the
  repository's own configuration. I have not audited which settings could make `git log`
  run other programs, so only point it at repositories you trust.
- Anything a tool returns is sent to whatever model the client is connected to.

## Layout

```
src/main.rs      argument handling, stdio transport (diagnostics go to stderr only)
src/server.rs    the three tools (#[tool_router]) and the server identity
src/sandbox.rs   the security boundary, with unit tests
tests/mcp.rs     end-to-end tests over the real protocol
```

## Tests

```bash
cargo test                      # 10 unit tests + 14 end-to-end tests
cargo fmt --check && cargo clippy --all-targets -- -D warnings
```

The end-to-end tests start the server on one end of an in-memory pipe and use the SDK's
own client on the other, so initialisation, tool listing, schemas and tool calls are
real MCP traffic. Two more tests run the compiled binary over stdio and check that
**stdout carries only JSON-RPC** (a stray `println!` would corrupt the protocol) and
that a bad project directory exits with status 2. CI runs everything on Linux and macOS.

Not verified: the official MCP Inspector and real desktop clients, and Windows.

## Known limitations

- Reads whole files up to the cap; no search, no glob filtering, no `.gitignore` handling.
- `git_log` only; no diff or blame.
- stdio only (no HTTP transport), single project directory per server process.
- The advertised version is a literal in `server.rs` (the macro needs one); a test fails if it
  drifts from `Cargo.toml`.

## Roadmap

- `search_text` (bounded grep) and `.gitignore`-aware listings.
- A configurable deny list.
- Compare with a TypeScript implementation of the same server.

## License

MIT
