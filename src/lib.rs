//! A small, read-only MCP server that lets an AI assistant look around a project:
//! list files, read text files and see recent git commits, all confined to one
//! directory (see [`sandbox`]).

pub mod sandbox;
pub mod server;

pub use sandbox::Sandbox;
pub use server::ProjectServer;
