//! Admitting a connection on what it presents and how every close says why.
//!
//! A route that resolves identity before the upgrade hands the transport an
//! [`Agent`] and the connection registers at once. A route that cannot, which
//! is every browser and mobile client since neither can set a header, hands it
//! a [`ConnectionAdmitter`] instead. The socket then waits on its own task,
//! unregistered, until a [`Kind::Credential`] frame arrives, the admitter
//! answers and the connection registers as whoever it said. Nothing crosses
//! an unadmitted socket in either direction: inbound, a data frame before the
//! credential closes it; outbound, a refusal is a [`Kind::Goodbye`] and
//! nothing else.
//!
//! Holding the socket on its task rather than in the registry was measured:
//! a parked socket costs about 23 KiB and 45 us where a registered one costs
//! about 48 KiB and 215 us and keeps costing probe CPU while idle, so a flood
//! of sockets that never present pays for nothing but the socket.
//!
//! [`Farewell`] is the one vocabulary for every server-initiated close: the
//! refusal here, the pending timer, [`ConnectionManager::close_connection`],
//! [`ConnectionManager::set_deadline`] and their agent-wide forms. Each writes
//! a `Goodbye` last and, on WebSocket, closes with the same code.
//!
//! [`ConnectionManager::close_connection`]: crate::manager::ConnectionManager::close_connection
//! [`ConnectionManager::set_deadline`]: crate::manager::ConnectionManager::set_deadline

use std::net::SocketAddr;

use async_trait::async_trait;
use plaza::agent::{Agent, AgentId};
use plaza_wire::frame::{self, Goodbye, Kind, ProtocolVersion};
use tracing::trace;

use crate::codec::WireCodec;
use crate::manager::OutboundFrame;

/// Why a connection is being closed, as the client will hear it.
///
/// `code` is a WebSocket close code and means the same thing on TCP, where the
/// [`Goodbye`] frame is the only place it travels. The application supplies
/// one on every close it orders; the two the session sends by itself are
/// [`credential_expected`](Self::credential_expected) and
/// [`credential_timeout`](Self::credential_timeout).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Farewell {
  pub code: u16,
  /// Anything worth saying beside the code, encoded however the application
  /// chooses. The session never reads it.
  pub detail: Option<Vec<u8>>,
}

impl Farewell {
  pub fn new(code: u16) -> Self {
    Self { code, detail: None }
  }

  pub fn with_detail(mut self, detail: impl Into<Vec<u8>>) -> Self {
    self.detail = Some(detail.into());
    self
  }

  /// A data frame arrived on a connection that had not presented a credential.
  pub fn credential_expected() -> Self {
    Self::new(Goodbye::CREDENTIAL_EXPECTED)
  }

  /// No credential arrived within [`Limits::credential_timeout`](crate::manager::Limits::credential_timeout).
  pub fn credential_timeout() -> Self {
    Self::new(Goodbye::CREDENTIAL_TIMEOUT)
  }

  /// The [`Kind::Goodbye`] frame a transport writes last before it closes.
  pub fn encode<C: WireCodec>(&self, codec: &C) -> OutboundFrame {
    let goodbye = Goodbye {
      code: self.code,
      detail: self.detail.clone(),
    };
    OutboundFrame::from(frame::encode_goodbye(codec, &goodbye).unwrap_or_else(|_| vec![Kind::Goodbye.as_byte()]))
  }
}

/// What the transport knows about a socket before anything has been presented.
///
/// Anything from the upgrade request itself, a forwarded-for header or a room
/// in the URL, is the route's to capture: build the admitter per request and
/// close over it.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Peer {
  pub addr: Option<SocketAddr>,
}

impl Peer {
  pub fn new(addr: Option<SocketAddr>) -> Self {
    Self { addr }
  }
}

/// The admitter's answer.
#[derive(Debug, Clone)]
pub enum ConnectionAdmission<ID: AgentId> {
  /// Register the connection as this agent. The join is announced from here.
  Admitted(Agent<ID>),
  /// Close the socket with this goodbye. Nothing was registered and nothing
  /// is announced.
  Refused(Farewell),
}

/// The gate a socket passes to enter the session and as whom.
///
/// Async because most identity lives in a store; one boxed future per
/// connection open is nothing next to the handshake that preceded it. Plaza
/// verifies nothing itself: what a credential is and what it proves are the
/// application's.
#[async_trait]
pub trait ConnectionAdmitter<ID: AgentId>: Send + Sync {
  async fn admit(&self, credential: &[u8], peer: &Peer) -> ConnectionAdmission<ID>;
}

/// What a frame read before admission leaves the connection task to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Presented {
  /// Nothing yet. Keep reading.
  Nothing,
  /// The credential, as it arrived. Ask the admitter.
  Credential(Vec<u8>),
  /// Close the socket with this goodbye.
  Refuse(Farewell),
}

/// The rules for a socket that has not been admitted, shared by every
/// transport. A custom transport drives one of these until it yields a
/// credential or a refusal.
#[derive(Debug)]
pub struct Pending {
  declared: Option<ProtocolVersion>,
  max_credential_bytes: usize,
}

impl Pending {
  pub fn new(max_credential_bytes: usize) -> Self {
    Self {
      declared: None,
      max_credential_bytes,
    }
  }

  /// The version the client's `Hello` declared, if one arrived. The transport
  /// records it once the connection is registered, since there is nothing to
  /// record it against before.
  pub fn declared(&self) -> Option<ProtocolVersion> {
    self.declared
  }

  /// One inbound frame. A probe is neither answered nor forwarded: answering
  /// would let an unadmitted peer spend the session's effort and there is no
  /// agent to time.
  pub fn on_frame<C: WireCodec>(&mut self, codec: &C, bytes: &[u8]) -> Presented {
    let Some((tag, body)) = frame::split(bytes) else {
      return Presented::Nothing;
    };
    match Kind::from_byte(tag) {
      Some(Kind::Hello) => {
        if let Ok(theirs) = codec.decode::<ProtocolVersion>(body) {
          self.declared = Some(theirs);
        }
        Presented::Nothing
      }
      Some(Kind::Credential) => {
        if body.len() > self.max_credential_bytes {
          Presented::Refuse(Farewell::new(MESSAGE_TOO_BIG))
        } else {
          Presented::Credential(body.to_vec())
        }
      }
      Some(Kind::Ops) => Presented::Refuse(Farewell::credential_expected()),
      Some(Kind::Ping) | Some(Kind::Pong) | Some(Kind::Goodbye) => Presented::Nothing,
      _ => {
        trace!(kind = tag, "Skipping a frame of unknown kind before admission.");
        Presented::Nothing
      }
    }
  }
}

/// RFC 6455's close code for a message too big to process.
pub const MESSAGE_TOO_BIG: u16 = 1009;
/// RFC 6455's close code for a peer that broke the rules, which is what a
/// connection ejected for exceeding its inbound rate hears.
pub const POLICY_VIOLATION: u16 = 1008;

#[cfg(test)]
mod tests {
  use super::*;
  use crate::codec::JsonCodec;

  fn framed(kind: Kind, body: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    frame::begin(kind, &mut buf);
    buf.extend_from_slice(body);
    buf
  }

  #[test]
  fn a_hello_is_kept_and_the_credential_is_handed_over_as_it_arrived() {
    let mut pending = Pending::new(64);
    let mut hello = Vec::new();
    frame::begin(Kind::Hello, &mut hello);
    JsonCodec.encode_into(&ProtocolVersion(7), &mut hello).unwrap();
    assert_eq!(pending.on_frame(&JsonCodec, &hello), Presented::Nothing);
    assert_eq!(pending.declared(), Some(ProtocolVersion(7)));

    assert_eq!(
      pending.on_frame(&JsonCodec, &framed(Kind::Credential, b"tok3n")),
      Presented::Credential(b"tok3n".to_vec())
    );
  }

  #[test]
  fn ops_before_the_credential_close_the_socket_and_probes_are_ignored() {
    let mut pending = Pending::new(64);
    assert_eq!(pending.on_frame(&JsonCodec, &framed(Kind::Ping, b"{}")), Presented::Nothing);
    assert_eq!(pending.on_frame(&JsonCodec, &framed(Kind::Pong, b"{}")), Presented::Nothing);
    assert_eq!(pending.on_frame(&JsonCodec, &[200, 1, 2]), Presented::Nothing, "unknown kinds are still skipped");
    assert_eq!(pending.on_frame(&JsonCodec, &[]), Presented::Nothing);
    assert_eq!(
      pending.on_frame(&JsonCodec, &framed(Kind::Ops, b"[]")),
      Presented::Refuse(Farewell::credential_expected())
    );
  }

  #[test]
  fn a_credential_over_the_cap_is_refused_as_too_big() {
    let mut pending = Pending::new(4);
    assert_eq!(
      pending.on_frame(&JsonCodec, &framed(Kind::Credential, b"12345")),
      Presented::Refuse(Farewell::new(MESSAGE_TOO_BIG))
    );
  }

  #[test]
  fn a_farewell_encodes_as_a_goodbye_frame() {
    let farewell = Farewell::new(4403).with_detail(b"banned".to_vec());
    let encoded = farewell.encode(&JsonCodec);
    let goodbye = frame::decode_goodbye(&JsonCodec, &encoded).unwrap();
    assert_eq!(goodbye.code, 4403);
    assert_eq!(goodbye.detail.as_deref(), Some(&b"banned"[..]));
    assert_eq!(Farewell::credential_timeout().code, 4408);
    assert_eq!(Farewell::credential_expected().code, 4401);
  }
}
