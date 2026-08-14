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
pub mod conditioner;
pub mod error;
pub mod host;
pub mod manager;
pub mod stats;
pub mod workload;

/// The frames a session answers for itself: latency probes and their
/// correlation.
///
/// Not gated on the shipped transports, deliberately: a transport that enables
/// neither still has a link plane to answer for, and gating this is what made
/// the seam a privilege rather than a surface.
pub mod control;

/// The connection loop a transport writes, minus the socket: the parts above
/// assembled the way plaza's own adapters assemble them.
pub mod driver;

pub use codec::WireCodec;
#[cfg(feature = "json")]
pub use codec::JsonCodec;
#[cfg(feature = "msgpack")]
pub use codec::MsgPackCodec;
pub use conditioner::{Delivery, DirectionProfile, LinkProfile, LinkPublisher, LinkSink, RETRANSMIT_PENALTY};
pub use driver::LinkDriver;
pub use error::SessionLayerError;
pub use workload::{Priority, Workload, DEFAULT_SOCKET_BUFFER_BYTES};
pub use manager::{
  ConnectionManager, ConnectionOrder, Frame, InboundOverflow, InboundVolume, Limits, OutboundOverflow, OutboundVolume, Overflow, PresenceOverflow,
  Probes, Queues, SessionClock, SessionOptions, TransportSession,
  DEFAULT_BROADCAST_CAPACITY, DEFAULT_CLIENT_QUEUE_CAPACITY, DEFAULT_CONDITIONER_CAPACITY,
  DEFAULT_MAX_FRAME_BYTES, DEFAULT_MAX_MESSAGE_BYTES, DEFAULT_PROBE_FAST_INTERVAL, DEFAULT_PROBE_FAST_PINGS,
  DEFAULT_PROBE_IDLE_INTERVAL, DEFAULT_PROBE_SLOTS,
};

#[cfg(feature = "actix_ws")]
pub mod actix_ws;
#[cfg(feature = "actix_ws")]
pub use actix_ws::ActixWsPlazaSession;

#[cfg(feature = "tcp")]
pub mod tcp;
#[cfg(feature = "tcp")]
pub use tcp::TcpPlazaSession;
