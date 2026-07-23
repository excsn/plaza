//! Plaza Session: `plaza::session::Session` implementations for real network
//! transports.
//!
//! Both transports share one connection manager, targeting implementation, and
//! serialization path (see [`manager`]); the per-transport modules are just
//! socket pumps. The wire format is pluggable via [`codec::WireCodec`]:
//! JSON by default, but an application can supply MessagePack or bincode.
//!
//! Enable the `actix_ws` and/or `tcp` features to select transports.

pub mod codec;
pub mod error;
pub mod manager;

pub use codec::{JsonCodec, WireCodec};
pub use error::SessionLayerError;
pub use manager::{ConnectionManager, TransportSession};

#[cfg(feature = "actix_ws")]
pub mod actix_ws;
#[cfg(feature = "actix_ws")]
pub use actix_ws::ActixWsPlazaSession;

#[cfg(feature = "tcp")]
pub mod tcp;
#[cfg(feature = "tcp")]
pub use tcp::TcpPlazaSession;
