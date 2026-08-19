//! The wire vocabulary: a window in, cells out.

use serde::{Deserialize, Serialize};

pub type WatcherId = u32;

pub const TICK_HZ: u64 = 30;

/// One datagram, one message: plaza's frame cannot be fragmented, so every
/// outbound frame must fit a conservative path MTU.
pub const MTU: usize = 1200;

/// What a `Cells` payload may spend of the MTU, leaving room for the frame
/// tag and the MessagePack envelope around it.
pub const PAYLOAD_BUDGET: usize = 1100;

/// World cell side in world units. Also the wire granularity: a packed ant is
/// a u8 pair of offsets inside its cell, so resolution is `CELL / 256`.
pub const CELL: f32 = 8.0;

/// World side in world units. 2040 over 8-unit cells makes a 256 x 256 board
/// (`CellSpace` adds a boundary row), so a cell index fits a u16 exactly.
pub const EXTENT: f32 = 2040.0;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum AntOp {
  /// Client to server: where the pane is. Repeating it is the keepalive.
  Window { x: f32, y: f32, half: f32 },
  /// Client to server: the `Welcome` landed, stop resending it.
  WelcomeSeen,
  /// Server to client, resent until seen: what the board is.
  Welcome {
    tick: u32,
    extent: f32,
    cell: f32,
    population: u32,
    nest: (f32, f32),
    sites: Vec<(f32, f32)>,
  },
  /// Server to client: concatenated cell records for the pane, whole cells
  /// only, complete state each time. A lost datagram costs freshness, never
  /// correctness.
  Cells { tick: u32, bytes: Packed },
  /// Server to everyone, once a second: the panel numbers, so an observer
  /// window shows the server's own accounting rather than modelling it.
  Stats(StatsSnapshot),
  /// Client to server: a live setting. Zero leaves a value as it is.
  Dial { ants: u32 },
}

/// One second of the server's tick, phase by phase. Milliseconds throughout.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct StatsSnapshot {
  pub ants: u32,
  pub watchers: u32,
  pub packed_cells: u32,
  pub delivered: u64,
  pub step_ms: f32,
  pub step_worst_ms: f32,
  pub rebuild_ms: f32,
  pub rebuild_worst_ms: f32,
  pub publish_ms: f32,
  pub publish_worst_ms: f32,
  pub assemble_ms: f32,
  pub assemble_worst_ms: f32,
  pub tick_mean_ms: f32,
  pub tick_worst_ms: f32,
  pub pps: f32,
  pub mbps: f32,
  pub send_busy_ms: f32,
  pub dropped: u64,
  pub body: String,
}

/// Cell payload bytes, packed once and refcounted into every datagram that
/// carries them.
#[derive(Clone, Debug, PartialEq)]
pub struct Packed(pub std::sync::Arc<Vec<u8>>);

impl Packed {
  pub fn new(bytes: Vec<u8>) -> Self {
    Self(std::sync::Arc::new(bytes))
  }

  pub fn as_slice(&self) -> &[u8] {
    &self.0
  }
}

impl Serialize for Packed {
  fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_bytes(&self.0)
  }
}

impl<'de> Deserialize<'de> for Packed {
  fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    struct Visitor;
    impl serde::de::Visitor<'_> for Visitor {
      type Value = Vec<u8>;
      fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("bytes")
      }
      fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<Vec<u8>, E> {
        Ok(v.to_vec())
      }
      fn visit_byte_buf<E: serde::de::Error>(self, v: Vec<u8>) -> Result<Vec<u8>, E> {
        Ok(v)
      }
    }
    d.deserialize_bytes(Visitor).map(Packed::new)
  }
}
