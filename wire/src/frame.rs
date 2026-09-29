//! Framing: the one byte in front of every message that says what it is.
//!
//! # Why the tag is not part of the encoded document
//!
//! A serde enum would express this too.
//! The *codec* then decides what the tag costs: under JSON a variant is a quoted
//! string (`{"Ops":...}`, four bytes of structure), under MessagePack an array
//! element, under protobuf a field number. A byte written ahead of the body
//! costs exactly one byte in every format and the decoder reads it without
//! parsing anything.
//!
//! Measured against a serde enum tag on the same message: 39 bytes against 42
//! and 113ns to decode against 180ns. The gap widens for the alternative that
//! keeps the tag inside the document *and* dispatches on it, which needs a
//! second parse of the body (239ns).
//!
//! # Skipping unknown kinds
//!
//! [`Kind::from_byte`] returns `None` for a tag this build does not know and
//! the transports **skip such a frame and carry on**. That rule has to exist
//! from the start: a client already deployed cannot learn to tolerate a new
//! frame kind later, so adding one is only safe if every peer was already built
//! to ignore what it does not recognise.
//!
//! This is also why the tag is read by hand rather than by `serde_repr`, which
//! errors on an unknown discriminant and would make the rule unexpressible.
//!
//! # What belongs in [`Kind`]
//!
//! A kind is an instruction to the *session*; [`Kind::Ops`] is the one kind
//! whose body belongs to the application. If application code has to act on a
//! proposed kind, make it an op instead.
//! `Hello` and `Ping` pass, because recording a version and echoing a value
//! are things a session can finish by itself. `Credential` passes because a
//! session holding a connection admitter can hold the socket, run a timer and
//! admit or close without the application seeing a frame. `Goodbye` passes
//! because the client library consumes it and hands the application a code.

/// What a frame carries.
///
/// Add a variant here to add a message kind. Old peers skip it (see the
/// module docs), so an addition does not break them. This file is outside the
/// vocabulary [`crate::build`] hashes, so a new kind does not move a
/// consumer's protocol version either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
#[non_exhaustive]
pub enum Kind {
  /// A batch of application operations. The body is `Vec<Op>`.
  Ops = 0,
  /// The protocol version this peer speaks. The body is a [`ProtocolVersion`].
  ///
  /// Sent once when a connection opens, by both ends, rather than on every
  /// frame: it cannot change mid-connection and carrying it per frame measured
  /// 53 bytes against 42 under JSON for no information gained.
  Hello = 1,
  /// A latency probe. The body is a [`Ping`] and the receiving session answers
  /// it with a [`Kind::Pong`] without the application being involved.
  Ping = 2,
  /// The answer to a [`Kind::Ping`]. The body is a [`Pong`].
  Pong = 3,
  /// What a client presents to be admitted. The body is opaque bytes that the
  /// session hands to its connection admitter without decoding; under a text
  /// codec it is text. Sent once, right after the client's [`Kind::Hello`].
  ///
  /// A session with no admitter skips it like any frame it has no use for.
  Credential = 4,
  /// Why a connection is ending. The body is a [`Goodbye`], written last
  /// before every close a server orders, on every transport. A WebSocket close
  /// frame carries the same code; TCP has nothing else.
  Goodbye = 5,
}

impl Kind {
  /// The tag byte written ahead of the body.
  pub const fn as_byte(self) -> u8 {
    self as u8
  }

  /// Reads a tag; `None` if this build does not know it.
  ///
  /// `None` means *skip the frame* and keep the connection: a peer speaking a
  /// newer protocol may send kinds this one has never heard of and refusing
  /// them turns every additive change into a break.
  pub const fn from_byte(byte: u8) -> Option<Self> {
    match byte {
      0 => Some(Kind::Ops),
      1 => Some(Kind::Hello),
      2 => Some(Kind::Ping),
      3 => Some(Kind::Pong),
      4 => Some(Kind::Credential),
      5 => Some(Kind::Goodbye),
      _ => None,
    }
  }
}

/// What a peer says it speaks, sent as the body of a [`Kind::Hello`] frame.
///
/// The number comes from [`crate::build`], which hashes the type definitions
/// that make up your wire format. Zero means "unknown": a peer that could not
/// compute one is never mistaken for a peer that agrees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ProtocolVersion(pub u32);

impl ProtocolVersion {
  pub const UNKNOWN: ProtocolVersion = ProtocolVersion(0);

  /// Whether two peers agree well enough to talk.
  ///
  /// An unknown version on either side is treated as agreement, because a peer
  /// that declares nothing is the pre-handshake case rather than a wrong one
  /// and refusing it would break every client built before this frame existed.
  pub const fn agrees_with(self, other: ProtocolVersion) -> bool {
    self.0 == 0 || other.0 == 0 || self.0 == other.0
  }
}

/// A latency probe, the body of a [`Kind::Ping`] frame.
///
/// # Units
///
/// Plaza never reads `origin` as a quantity: it comes back in the [`Pong`]
/// exactly as it went out and only the sender ever interprets it. Stamp it with
/// milliseconds, nanoseconds, a frame counter or a sequence number and document
/// the choice wherever your application documents its protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Ping {
  pub origin: u64,
}

/// The answer to a [`Ping`], the body of a [`Kind::Pong`] frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Pong {
  /// The probe's `origin`, echoed back unread.
  pub origin: u64,
  /// The responder's clock when the reply was built, in the responder's own
  /// unit; `None` if it has no clock to offer. Which clock this reads and in
  /// what unit is agreed out of band: the two ends have to use the same one for
  /// an offset computed from it to be meaningful.
  pub responder: Option<u64>,
}

/// Why a connection is ending, the body of a [`Kind::Goodbye`] frame.
///
/// `code` is a WebSocket close code and means the same thing on a transport
/// that has no close frame. RFC 6455 gives 4000 to 4999 to the application;
/// the two the session sends itself are the associated constants below. A
/// client library turns this frame into its disconnected event and the
/// application never handles it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Goodbye {
  pub code: u16,
  /// Anything the server wants to say beside the code, encoded however the
  /// application chooses; the session does not read it.
  pub detail: Option<Vec<u8>>,
}

impl Goodbye {
  /// A data frame arrived on a connection that had not presented a
  /// credential yet.
  pub const CREDENTIAL_EXPECTED: u16 = 4401;
  /// No credential arrived within the session's pending timeout.
  pub const CREDENTIAL_TIMEOUT: u16 = 4408;
}

/// Encodes a [`Goodbye`] as one [`Kind::Goodbye`] frame.
#[cfg(feature = "serde")]
pub fn encode_goodbye<C: crate::WireCodec>(
  codec: &C,
  goodbye: &Goodbye,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
  let mut out = Vec::with_capacity(PROBE_FRAME_HINT);
  begin(Kind::Goodbye, &mut out);
  codec.encode_into(goodbye, &mut out)?;
  Ok(out)
}

/// Decodes a frame's [`Goodbye`] or `None` when the frame is not
/// [`Kind::Goodbye`] or its body does not decode.
#[cfg(feature = "serde")]
pub fn decode_goodbye<C: crate::WireCodec>(codec: &C, frame: &[u8]) -> Option<Goodbye> {
  let (tag, body) = split(frame)?;
  if Kind::from_byte(tag) != Some(Kind::Goodbye) {
    return None;
  }
  codec.decode::<Goodbye>(body).ok()
}

/// Builds the [`Kind::Pong`] frame answering a ping, or `None` if `ping_body`
/// does not decode.
///
/// `responder` is the local clock in the local unit, if there is one to offer.
#[cfg(feature = "serde")]
pub fn answer_ping<C: crate::WireCodec>(codec: &C, ping_body: &[u8], responder: Option<u64>) -> Option<Vec<u8>> {
  let ping = codec.decode::<Ping>(ping_body).ok()?;
  let mut buf = Vec::with_capacity(PROBE_FRAME_HINT);
  begin(Kind::Pong, &mut buf);
  codec
    .encode_into(
      &Pong {
        origin: ping.origin,
        responder,
      },
      &mut buf,
    )
    .ok()?;
  Some(buf)
}

/// Encodes a batch of ops as one [`Kind::Ops`] frame: the kind byte, then
/// the codec's one document. The body is the ops array itself; who sent it is
/// the server's bookkeeping and is not on the wire.
#[cfg(feature = "serde")]
pub fn encode_ops<C: crate::WireCodec, Op: serde::Serialize>(
  codec: &C,
  ops: &[Op],
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
  let mut out = Vec::new();
  begin(Kind::Ops, &mut out);
  codec.encode_into(&ops, &mut out)?;
  Ok(out)
}

/// Decodes a frame's ops, or `None` when the frame is not [`Kind::Ops`] or
/// its body does not decode. Skipping either case silently is the same rule
/// unknown kinds get; a client that must tell them apart splits by hand.
#[cfg(feature = "serde")]
pub fn decode_ops<C: crate::WireCodec, Op: serde::de::DeserializeOwned>(codec: &C, frame: &[u8]) -> Option<Vec<Op>> {
  let (tag, body) = split(frame)?;
  if Kind::from_byte(tag) != Some(Kind::Ops) {
    return None;
  }
  codec.decode::<Vec<Op>>(body).ok()
}

/// Splits a frame into its kind byte and its body.
///
/// Returns `None` for an empty frame, which is malformed rather than unknown.
/// A known-shape frame with an unrecognised kind still splits: deciding what to
/// do about the kind is [`Kind::from_byte`]'s job.
pub fn split(frame: &[u8]) -> Option<(u8, &[u8])> {
  frame.split_first().map(|(kind, body)| (*kind, body))
}

/// Enough for a `Ping` or a `Pong` under either shipped codec, so a probe frame
/// is one allocation rather than the four or five a `Vec` growing from nothing
/// costs to reach twenty-odd bytes.
pub const PROBE_FRAME_HINT: usize = 64;

/// Starts a frame: writes the tag, so the body can be appended after it.
///
/// Writing the tag first is why [`crate::WireCodec::encode_into`] appends
/// rather than returning a `Vec`. Inserting a byte at the front of an
/// encoded body would shift every byte of it.
///
/// Clears first, so a buffer being reused starts a frame rather than extending
/// the last one. Capacity survives a clear, so a reused buffer does not
/// reallocate.
pub fn begin(kind: Kind, buf: &mut Vec<u8>) {
  buf.clear();
  buf.push(kind.as_byte());
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_frame_is_one_tag_byte_and_a_body() {
    let mut buf = Vec::new();
    begin(Kind::Ops, &mut buf);
    buf.extend_from_slice(b"body");
    assert_eq!(buf.len(), 5, "one byte of framing, whatever the codec");
    let (kind, body) = split(&buf).expect("a non-empty frame splits");
    assert_eq!(Kind::from_byte(kind), Some(Kind::Ops));
    assert_eq!(body, b"body");
  }

  #[test]
  fn an_unknown_kind_is_skippable_rather_than_fatal() {
    // A peer built before a new frame kind existed must be able to ignore it,
    // which it can only do if this returns None instead of erroring.
    assert_eq!(Kind::from_byte(6), None, "the first unassigned byte");
    assert_eq!(Kind::from_byte(200), None);
    let frame = [200u8, 1, 2, 3];
    let (kind, body) = split(&frame).expect("still a well-formed frame");
    assert_eq!(Kind::from_byte(kind), None, "unknown, so the frame is skipped");
    assert_eq!(body, &[1, 2, 3], "and its body is still delimited");
  }

  #[test]
  fn a_hello_dispatches_to_a_different_body_than_ops() {
    // The body type follows from the kind, so a protocol frame does not have
    // to fit into the application's ops.
    assert_eq!(Kind::from_byte(Kind::Hello.as_byte()), Some(Kind::Hello));
    assert_ne!(Kind::Ops.as_byte(), Kind::Hello.as_byte());
  }

  #[test]
  fn every_kind_survives_its_own_tag_byte() {
    // Each kind, through the framing it will actually be written with. The
    // probe tests below assert a Pong is produced; this asserts the tags
    // themselves round-trip, which is what a peer dispatches on.
    for kind in [Kind::Ops, Kind::Hello, Kind::Ping, Kind::Pong, Kind::Credential, Kind::Goodbye] {
      let mut buf = Vec::new();
      begin(kind, &mut buf);
      buf.extend_from_slice(b"body");
      let (tag, body) = split(&buf).expect("a non-empty frame splits");
      assert_eq!(Kind::from_byte(tag), Some(kind), "{kind:?} round-trips its tag");
      assert_eq!(body, b"body");
    }
    // And the tags are distinct, otherwise dispatch is ambiguous.
    let bytes = [Kind::Ops, Kind::Hello, Kind::Ping, Kind::Pong, Kind::Credential, Kind::Goodbye].map(Kind::as_byte);
    assert_eq!(bytes, [0, 1, 2, 3, 4, 5], "wire values are pinned; renumbering breaks every peer");
  }

  #[test]
  fn an_undeclared_version_agrees_with_everything() {
    // A peer built before the handshake existed sends no Hello at all, so it
    // must not be refused for failing to match.
    assert!(ProtocolVersion::UNKNOWN.agrees_with(ProtocolVersion(7)));
    assert!(ProtocolVersion(7).agrees_with(ProtocolVersion::UNKNOWN));
    assert!(ProtocolVersion(7).agrees_with(ProtocolVersion(7)));
    assert!(!ProtocolVersion(7).agrees_with(ProtocolVersion(8)));
  }

  #[test]
  fn an_empty_frame_is_malformed() {
    assert_eq!(split(&[]), None);
  }

  #[cfg(feature = "json")]
  mod probes {
    use super::*;
    use crate::{JsonCodec, WireCodec};

    #[test]
    fn a_pong_echoes_the_origin_it_was_given() {
      let mut ping = Vec::new();
      begin(Kind::Ping, &mut ping);
      JsonCodec.encode_into(&Ping { origin: 987_654_321 }, &mut ping).unwrap();

      let (_, body) = split(&ping).unwrap();
      let reply = answer_ping(&JsonCodec, body, Some(42)).expect("a well-formed ping is answerable");

      let (kind, body) = split(&reply).unwrap();
      assert_eq!(Kind::from_byte(kind), Some(Kind::Pong));
      let pong: Pong = JsonCodec.decode(body).unwrap();
      assert_eq!(pong.origin, 987_654_321, "the origin comes back unread");
      assert_eq!(pong.responder, Some(42));
    }

    #[test]
    fn a_responder_without_a_clock_offers_nothing() {
      // Not zero: zero is a legitimate clock reading and a responder that has
      // no clock has to be distinguishable from one whose clock reads zero.
      let mut ping = Vec::new();
      begin(Kind::Ping, &mut ping);
      JsonCodec.encode_into(&Ping { origin: 1 }, &mut ping).unwrap();
      let (_, body) = split(&ping).unwrap();

      let reply = answer_ping(&JsonCodec, body, None).unwrap();
      let (_, body) = split(&reply).unwrap();
      assert_eq!(JsonCodec.decode::<Pong>(body).unwrap().responder, None);
    }

    #[test]
    fn a_malformed_ping_is_unanswerable_rather_than_fatal() {
      assert!(answer_ping(&JsonCodec, b"not a ping", None).is_none());
    }
  }

  #[cfg(feature = "msgpack")]
  #[test]
  fn a_pong_without_a_clock_survives_msgpack() {
    use crate::{MsgPackCodec, WireCodec};
    let pong = Pong { origin: 7, responder: None };
    let mut buf = Vec::new();
    MsgPackCodec.encode_into(&pong, &mut buf).unwrap();
    assert_eq!(MsgPackCodec.decode::<Pong>(&buf).unwrap(), pong);
  }

  #[test]
  #[cfg(feature = "json")]
  fn a_goodbye_carries_its_code_and_is_not_ops() {
    use crate::JsonCodec;
    let goodbye = Goodbye {
      code: Goodbye::CREDENTIAL_TIMEOUT,
      detail: Some(b"be quicker".to_vec()),
    };
    let frame = encode_goodbye(&JsonCodec, &goodbye).unwrap();
    assert_eq!(frame[0], Kind::Goodbye.as_byte());
    assert_eq!(decode_goodbye(&JsonCodec, &frame), Some(goodbye));
    assert_eq!(decode_ops::<_, String>(&JsonCodec, &frame), None);

    let bare = encode_goodbye(&JsonCodec, &Goodbye { code: 1000, detail: None }).unwrap();
    assert_eq!(decode_goodbye(&JsonCodec, &bare).unwrap().detail, None);
  }

  #[test]
  #[cfg(feature = "json")]
  fn ops_ride_one_frame_and_other_kinds_decode_to_none() {
    use crate::JsonCodec;
    let frame = encode_ops(&JsonCodec, &["a", "b"]).unwrap();
    assert_eq!(frame[0], Kind::Ops.as_byte());
    assert_eq!(decode_ops::<_, String>(&JsonCodec, &frame), Some(vec!["a".to_owned(), "b".to_owned()]));

    let mut hello = Vec::new();
    begin(Kind::Hello, &mut hello);
    assert_eq!(decode_ops::<_, String>(&JsonCodec, &hello), None, "not ops, not an error");
  }
}
