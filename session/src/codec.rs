//! Wire format used by a transport.
//!
//! All encoding and decoding in this crate goes through a `WireCodec`, so an
//! application can pick its own format (JSON for browser debugging, MessagePack
//! or bincode for production) without touching transport code.
//!
//! The trait itself lives in [`plaza_wire`], which has no async dependencies,
//! so a client can implement the same format without pulling in a server
//! runtime. It is re-exported here because server code has no reason to name a
//! second crate.

pub use plaza_wire::WireCodec;

#[cfg(feature = "json")]
pub use plaza_wire::JsonCodec;

#[cfg(feature = "msgpack")]
pub use plaza_wire::{MsgPackCodec, MsgPackNamedCodec};
