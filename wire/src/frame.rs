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
      _ => None,
    }
  }
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
    assert_eq!(Kind::from_byte(200), None);
    let frame = [200u8, 1, 2, 3];
    let (kind, body) = split(&frame).expect("still a well-formed frame");
    assert_eq!(Kind::from_byte(kind), None, "unknown, so the frame is skipped");
    assert_eq!(body, &[1, 2, 3], "and its body is still delimited");
  }

  #[test]
  fn an_empty_frame_is_malformed() {
    assert_eq!(split(&[]), None);
  }
}
