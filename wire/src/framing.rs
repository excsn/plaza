//! Length-delimited stream framing: how frames ride a transport with no
//! message boundaries.
//!
//! A WebSocket hands each message over whole, so the [`frame`](crate::frame)
//! layer starts at the kind byte. TCP and its relatives hand over a byte
//! stream, so both ends must first agree where one frame ends and the next
//! begins. The contract is **a 4-byte big-endian length, then that many bytes
//! of frame**, the length not counting the prefix itself. This is also the
//! layout `tokio-util`'s `LengthDelimitedCodec` speaks by default, which is
//! what `plaza_session`'s TCP transport uses; this module is the same
//! contract named where both ends can see it, with a decoder for clients and
//! adapters that own their own I/O.
//!
//! Nothing here reads or writes a socket. Whoever owns the stream reads bytes
//! and [`feed`](LengthDelimited::feed)s them in; frames come out as they
//! complete:
//!
//! ```ignore
//! let mut framing = LengthDelimited::new(limits.max_frame_bytes);
//! let mut chunk = [0u8; 8192];
//! loop {
//!   while let Some(frame) = framing.next_frame()? {
//!     handle(&frame);
//!   }
//!   let n = stream.read(&mut chunk).await?;
//!   if n == 0 { return Ok(()); }
//!   framing.feed(&chunk[..n]);
//! }
//! ```

/// The bytes ahead of every frame on a stream transport.
pub const LENGTH_PREFIX_BYTES: usize = 4;

/// Appends `frame` behind its length prefix, one buffer for one write.
pub fn delimit(frame: &[u8], out: &mut Vec<u8>) {
  out.extend_from_slice(&length_prefix(frame.len()));
  out.extend_from_slice(frame);
}

/// The prefix alone, for a writer that sends prefix and frame separately.
pub fn length_prefix(frame_len: usize) -> [u8; LENGTH_PREFIX_BYTES] {
  (frame_len as u32).to_be_bytes()
}

/// A declared length past the limit. Not recoverable: a stream is only
/// re-synchronisable by its lengths, so once one cannot be trusted the only
/// safe move is to drop the connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Oversize {
  pub frame_bytes: usize,
  pub max_frame_bytes: usize,
}

impl std::fmt::Display for Oversize {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "frame of {} bytes over the {}-byte limit", self.frame_bytes, self.max_frame_bytes)
  }
}

impl std::error::Error for Oversize {}

/// The reading half of the contract: feed bytes as the stream delivers them,
/// take frames as they complete. No I/O and no runtime, so it serves a tokio
/// client, a blocking one, and a transport adapter alike.
///
/// The limit is required rather than defaulted: it is the reader's protection
/// against a peer declaring a gigabyte, and how much to tolerate is policy.
/// `plaza_session`'s `Limits::max_frame_bytes` is the same number on the
/// server side.
#[derive(Clone, Debug)]
pub struct LengthDelimited {
  buf: Vec<u8>,
  max_frame_bytes: usize,
}

impl LengthDelimited {
  pub fn new(max_frame_bytes: usize) -> Self {
    Self {
      buf: Vec::new(),
      max_frame_bytes,
    }
  }

  /// Bytes as the stream delivered them, boundaries nowhere in particular.
  pub fn feed(&mut self, bytes: &[u8]) {
    self.buf.extend_from_slice(bytes);
  }

  /// The next complete frame, `None` until more bytes arrive, or
  /// [`Oversize`] when the declared length cannot be honoured. Call in a loop:
  /// one feed can complete any number of frames.
  pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, Oversize> {
    if self.buf.len() < LENGTH_PREFIX_BYTES {
      return Ok(None);
    }
    let declared = u32::from_be_bytes(self.buf[..LENGTH_PREFIX_BYTES].try_into().expect("four bytes")) as usize;
    if declared > self.max_frame_bytes {
      return Err(Oversize {
        frame_bytes: declared,
        max_frame_bytes: self.max_frame_bytes,
      });
    }
    if self.buf.len() < LENGTH_PREFIX_BYTES + declared {
      return Ok(None);
    }
    let frame = self.buf[LENGTH_PREFIX_BYTES..LENGTH_PREFIX_BYTES + declared].to_vec();
    self.buf.drain(..LENGTH_PREFIX_BYTES + declared);
    Ok(Some(frame))
  }

  /// Bytes held waiting for a boundary.
  pub fn buffered(&self) -> usize {
    self.buf.len()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_frame_survives_the_round_trip() {
    let mut wire = Vec::new();
    delimit(b"hello", &mut wire);
    delimit(b"", &mut wire);
    delimit(b"world", &mut wire);

    let mut framing = LengthDelimited::new(64);
    framing.feed(&wire);
    assert_eq!(framing.next_frame(), Ok(Some(b"hello".to_vec())));
    assert_eq!(framing.next_frame(), Ok(Some(Vec::new())), "an empty frame is a frame");
    assert_eq!(framing.next_frame(), Ok(Some(b"world".to_vec())));
    assert_eq!(framing.next_frame(), Ok(None));
    assert_eq!(framing.buffered(), 0);
  }

  #[test]
  fn boundaries_fall_wherever_the_stream_puts_them() {
    // The reason the decoder exists: a read returns whatever the network
    // coughed up, one byte at a time being the adversarial case.
    let mut wire = Vec::new();
    delimit(b"split across reads", &mut wire);

    let mut framing = LengthDelimited::new(64);
    let mut frames = Vec::new();
    for byte in wire {
      framing.feed(&[byte]);
      while let Some(frame) = framing.next_frame().unwrap() {
        frames.push(frame);
      }
    }
    assert_eq!(frames, vec![b"split across reads".to_vec()]);
  }

  #[test]
  fn a_declared_length_past_the_limit_is_an_error_not_an_allocation() {
    let mut framing = LengthDelimited::new(16);
    framing.feed(&length_prefix(1_000_000));
    assert_eq!(
      framing.next_frame(),
      Err(Oversize {
        frame_bytes: 1_000_000,
        max_frame_bytes: 16
      }),
      "the lie is caught before any body bytes arrive"
    );
  }

  #[test]
  fn the_writer_helpers_agree_with_each_other() {
    let mut buffered = Vec::new();
    delimit(b"abc", &mut buffered);
    let mut separate = length_prefix(3).to_vec();
    separate.extend_from_slice(b"abc");
    assert_eq!(buffered, separate);
  }
}
