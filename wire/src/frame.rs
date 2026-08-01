//! Framing: the one byte in front of every message that says what it is.
//!
//! # Why the tag is not part of the encoded document
//!
//! A serde enum would express this too, and it is what the envelope used to be.
//! The problem is that the *codec* then decides what the tag costs: under JSON a
//! variant is a quoted string (`{"Ops":...}`, four bytes of structure), under
//! MessagePack an array element, under protobuf a field number. A byte written
//! ahead of the body costs exactly one byte in every format, and the decoder
//! reads it without parsing anything.
//!
//! Measured against a serde enum tag on the same message: 39 bytes against 42,
//! and 113ns to decode against 180ns. The gap widens for the alternative that
//! keeps the tag inside the document *and* dispatches on it, which needs a
//! second parse of the body (239ns).
//!
//! # Forward compatibility is a decision, not a property
//!
//! [`Kind::from_byte`] returns `None` for a tag this build does not know, and
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
//! A kind is an instruction to the *session*, and [`Kind::Ops`] is the one
//! whose body belongs to the application instead. The test for a proposed
//! kind: if application code has to act on it, it is an op and not a kind.
//! `Hello` and `Ping` pass, because recording a version and echoing a value
//! are things a session can finish by itself.

/// What a frame carries.
///
/// Add a variant here to add a message kind. Old peers will skip it (see the
/// module docs), so an addition does not break them, though the protocol hash
/// from [`crate::build`] will still change and ask them to reload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Kind {
  /// A batch of application operations. The body is `Vec<Op>`.
  Ops = 0,
  /// The protocol version this peer speaks. The body is a [`ProtocolVersion`].
  ///
  /// Sent once when a connection opens, by both ends, rather than on every
  /// frame: it cannot change mid-connection, and carrying it per frame measured
  /// 53 bytes against 42 under JSON for no information gained.
  Hello = 1,
  /// A latency probe. The body is a [`Ping`], and the receiving session answers
  /// it with a [`Kind::Pong`] without the application being involved.
  Ping = 2,
  /// The answer to a [`Kind::Ping`]. The body is a [`Pong`].
  Pong = 3,
}

impl Kind {
  /// The tag byte written ahead of the body.
  pub const fn as_byte(self) -> u8 {
    self as u8
  }

  /// Reads a tag, or `None` if this build does not know it.
  ///
  /// `None` means *skip the frame*, not *fail the connection*: a peer speaking
  /// a newer protocol may send kinds this one has never heard of, and refusing
  /// them turns every additive change into a break.
  pub const fn from_byte(byte: u8) -> Option<Self> {
    match byte {
      0 => Some(Kind::Ops),
      1 => Some(Kind::Hello),
      2 => Some(Kind::Ping),
      3 => Some(Kind::Pong),
      _ => None,
    }
  }
}

/// What a peer says it speaks, sent as the body of a [`Kind::Hello`] frame.
///
/// The number comes from [`crate::build`], which hashes the type definitions
/// that make up your wire format. Zero means "unknown": a peer that could not
/// compute one is never mistaken for a peer that agrees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProtocolVersion(pub u32);

impl ProtocolVersion {
  pub const UNKNOWN: ProtocolVersion = ProtocolVersion(0);

  /// Whether two peers agree well enough to talk.
  ///
  /// An unknown version on either side is treated as agreement, because a peer
  /// that declares nothing is the pre-handshake case rather than a wrong one,
  /// and refusing it would break every client built before this frame existed.
  pub const fn agrees_with(self, other: ProtocolVersion) -> bool {
    self.0 == 0 || other.0 == 0 || self.0 == other.0
  }
}

/// A latency probe, the body of a [`Kind::Ping`] frame.
///
/// # Units are the sender's business
///
/// Plaza never reads `origin` as a quantity: it comes back in the [`Pong`]
/// exactly as it went out, and only the sender ever interprets it. Stamp it
/// with milliseconds, nanoseconds, a frame counter, or a sequence number, and
/// document the choice wherever your application documents its protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Ping {
  pub origin: u64,
}

/// The answer to a [`Ping`], the body of a [`Kind::Pong`] frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Pong {
  /// The probe's `origin`, echoed back unread.
  pub origin: u64,
  /// The responder's clock when the reply was built, in the responder's own
  /// unit, or `None` if it has no clock to offer. Which clock this reads, and
  /// in what unit, is agreed out of band: the two ends have to mean the same
  /// one for an offset computed from it to mean anything.
  pub responder: Option<u64>,
}

/// Builds the [`Kind::Pong`] frame answering a ping, or `None` if `ping_body`
/// does not decode.
///
/// `responder` is the local clock in the local unit, if there is one to offer.
pub fn answer_ping<C: crate::WireCodec>(codec: &C, ping_body: &[u8], responder: Option<u64>) -> Option<Vec<u8>> {
  let ping = codec.decode::<Ping>(ping_body).ok()?;
  let mut buf = Vec::new();
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

/// Splits a frame into its kind byte and its body.
///
/// Returns `None` for an empty frame, which is malformed rather than unknown.
/// A known-shape frame with an unrecognised kind still splits: deciding what to
/// do about the kind is [`Kind::from_byte`]'s job.
pub fn split(frame: &[u8]) -> Option<(u8, &[u8])> {
  frame.split_first().map(|(kind, body)| (*kind, body))
}

/// Starts a frame: writes the tag, so the body can be appended after it.
///
/// Writing the tag first is the whole reason [`crate::WireCodec::encode_into`]
/// appends rather than returning a `Vec`. Inserting a byte at the front of an
/// encoded body would shift every byte of it.
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
    // The property a future frame kind depends on. A peer built before that
    // kind existed must be able to ignore it, and it can only do that if this
    // returns None instead of erroring.
    assert_eq!(Kind::from_byte(4), None, "the first unassigned byte");
    assert_eq!(Kind::from_byte(200), None);
    let frame = [200u8, 1, 2, 3];
    let (kind, body) = split(&frame).expect("still a well-formed frame");
    assert_eq!(Kind::from_byte(kind), None, "unknown, so the frame is skipped");
    assert_eq!(body, &[1, 2, 3], "and its body is still delimited");
  }

  #[test]
  fn a_hello_dispatches_to_a_different_body_than_ops() {
    // The reason the tag is worth its byte: the body type follows from the
    // kind, so a protocol frame is not squeezed into the application's ops.
    assert_eq!(Kind::from_byte(Kind::Hello.as_byte()), Some(Kind::Hello));
    assert_ne!(Kind::Ops.as_byte(), Kind::Hello.as_byte());
  }

  #[test]
  fn every_kind_survives_its_own_tag_byte() {
    // Each kind, through the framing it will actually be written with. The
    // probe tests below assert a Pong is produced; this asserts the tags
    // themselves round-trip, which is what a peer dispatches on.
    for kind in [Kind::Ops, Kind::Hello, Kind::Ping, Kind::Pong] {
      let mut buf = Vec::new();
      begin(kind, &mut buf);
      buf.extend_from_slice(b"body");
      let (tag, body) = split(&buf).expect("a non-empty frame splits");
      assert_eq!(Kind::from_byte(tag), Some(kind), "{kind:?} round-trips its tag");
      assert_eq!(body, b"body");
    }
    // And the tags are distinct, or dispatch is a coin flip.
    let bytes = [Kind::Ops, Kind::Hello, Kind::Ping, Kind::Pong].map(Kind::as_byte);
    assert_eq!(bytes, [0, 1, 2, 3], "wire values are pinned; renumbering breaks every peer");
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
      // Not zero: zero is a legitimate clock reading, and a responder that has
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
}
