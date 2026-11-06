//! MCP server surface: tool catalog as data, five profiles and a stdio JSON-RPC server.
//!
//! Ported from the reference package `spiderhead/mcp/` (FastMCP-based). This crate owns
//! everything an MCP client sees — `initialize`, `tools/list`, `tools/call`,
//! progress/cancel handling — while tool bodies live in domain crates that plug in
//! through [`handler::Registry`]. Until they do, every call answers
//! [`handler::NotYetPorted`].
//!
//! The catalog (`data/tools.json`) is generated from the reference baseline by
//! the reference suite; serving it verbatim is what keeps `tools/list`
//! divergence-free. One stdio server serves both surfaces; the CLI calls the
//! same handler functions the MCP `tools/call` path does.

pub mod catalog;
pub mod discovery;
pub mod error;
pub mod handler;
pub mod handlers;
pub mod profile;
pub mod protocol;
pub mod server;
pub mod stdio;
pub mod tool_layer;
pub mod transport;
pub mod validation;
