/// Styx (9P2000) protocol implementation for HeavenOS.
///
/// Styx is the Plan 9 / Inferno file protocol. Every system resource is
/// exposed as a file in a synthetic namespace. Agents interact with the
/// kernel by reading and writing these files over the Styx protocol.
///
/// This module implements:
/// - 9P2000 message parsing and serialization
/// - A synthetic file tree (no on-disk files — all generated on read)
/// - The /db/ctl SQL interface (Styx → SQLite)
mod message;
mod server;
pub mod namespace;

pub use message::{StyxMsg, StyxMsgType, NOTAG, NOFID};
pub use server::StyxServer;
pub use namespace::{Node, NodeKind};
