//! What crosses the wire and what it costs.
//!
//! The cost accounting is in [`wire_cost`]. This example's traffic splits
//! cleanly into a half that is derived and a half that cannot be, so it can
//! price both.

use serde::{Deserialize, Serialize};

use crate::sim::curtain::{Downed, Wave};
use crate::sim::types::{DeathRule, Dir8, PlayerBullet, PlayerId, Ship};

include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));

pub const PROTOCOL: u32 = WIRE_PROTOCOL;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerPolicy {
  pub sync_hz: u32,
  pub playout_delay_ms: u64,
  pub render_delay_ms: u64,
  pub input_max_late_ticks: u64,
  pub input_max_early_ticks: u64,
  /// Who may say a ship died. A client that guessed this wrong would either
  /// declare hits nobody asked for or wait for a verdict that never comes.
  pub death_rule: DeathRule,
  pub players: usize,
}

/// The half of the world that has to be sent.
///
/// Ships and player bullets only. Every enemy bullet on the screen is absent
/// from this by construction and the panel shows what that saves.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Frame {
  pub server_time_ms: u64,
  pub tick: u64,
  pub ships: Vec<Ship>,
  pub bullets: Vec<PlayerBullet>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeathVerdict {
  /// The ship said it was hit and the server recomputed the curtain and agreed.
  Confirmed,
  /// The ship said it was hit and the curtain at that tick says otherwise.
  ///
  /// Refusing prevents abuse. A ship that can declare its own death can
  /// declare it at a moment that suits it. With a shared curtain the server can
  /// check it for the price of one function call.
  Refused,
  /// The server found the contact itself, because the rule does not ask.
  ServerFound,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeathEvent {
  pub victim: PlayerId,
  pub at_tick: u64,
  pub at_ms: u64,
  pub lives_left: u32,
  pub verdict: DeathVerdict,
  /// Ticks between the contact and the server acting on it.
  ///
  /// Under `ServerOnly` this is the round trip, which makes the rule
  /// unplayable: you watched the bullet miss and still died. A death cannot be
  /// eased or undone.
  pub late_by_ticks: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Intent {
  Move(Dir8),
  Fire,
  /// The client's claim that it was hit on the tick it named.
  Struck,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Start {
  pub server_time_ms: u64,
  pub tick: u64,
  pub ships: Vec<Ship>,
  /// Every wave already in flight, so a joiner's curtain is complete from its
  /// first frame rather than filling in as waves happen to restart.
  pub waves: Vec<Wave>,
  pub downed: Vec<Downed>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Op {
  // ---- client to server ----
  Move { seq: u64, tick: u64, dir: Dir8 },
  Fire { seq: u64, tick: u64 },
  /// The client's claim that it was hit on this tick. Judged or trusted
  /// according to the rule.
  Struck { seq: u64, tick: u64 },

  // ---- server to client ----
  Welcome { player: PlayerId, policy: ServerPolicy, start: Box<Start> },
  Frame(Box<Frame>),
  /// A whole wave of the curtain, once. About a hundred bytes for what
  /// becomes hundreds of bullets over the next fifteen seconds.
  WaveUp(Box<Wave>),
  ArmDown(Downed),
  Died(Box<DeathEvent>),
  InputAck { seq: u64 },
  NoSeat { seats: usize },
  Refused { measured_one_way_ms: u64, allowed_one_way_ms: u64 },
}

impl Op {
  pub fn is_upstream(&self) -> bool {
    matches!(self, Op::Move { .. } | Op::Fire { .. } | Op::Struck { .. })
  }
}

/// What a frame of traffic actually costs, split by what produced it.
pub mod wire_cost {
  use super::*;

  /// The same ops with every variant renamed to a number.
  ///
  /// Used to measure **the share of a frame taken up by variant names**, which
  /// nothing else in this repository measures. The share is largest in a shmup, because a stream of tiny messages is mostly tag.
  ///
  /// Borrowed rather than converted, so measuring costs no clones and measures
  /// exactly what is sent.
  #[derive(Serialize)]
  pub enum Tagged<'a> {
    #[serde(rename = "0")]
    Move { seq: u64, tick: u64, dir: &'a Dir8 },
    #[serde(rename = "1")]
    Fire { seq: u64, tick: u64 },
    #[serde(rename = "2")]
    Struck { seq: u64, tick: u64 },
    #[serde(rename = "3")]
    Welcome { player: PlayerId, policy: &'a ServerPolicy, start: &'a Start },
    #[serde(rename = "4")]
    Frame(&'a Frame),
    #[serde(rename = "5")]
    WaveUp(&'a Wave),
    #[serde(rename = "6")]
    ArmDown(&'a Downed),
    #[serde(rename = "7")]
    Died(&'a DeathEvent),
    #[serde(rename = "8")]
    InputAck { seq: u64 },
    #[serde(rename = "9")]
    NoSeat { seats: usize },
    #[serde(rename = "a")]
    Refused { measured_one_way_ms: u64, allowed_one_way_ms: u64 },
  }

  impl<'a> From<&'a Op> for Tagged<'a> {
    fn from(op: &'a Op) -> Self {
      match op {
        Op::Move { seq, tick, dir } => Tagged::Move { seq: *seq, tick: *tick, dir },
        Op::Fire { seq, tick } => Tagged::Fire { seq: *seq, tick: *tick },
        Op::Struck { seq, tick } => Tagged::Struck { seq: *seq, tick: *tick },
        Op::Welcome { player, policy, start } => Tagged::Welcome { player: *player, policy, start },
        Op::Frame(f) => Tagged::Frame(f),
        Op::WaveUp(w) => Tagged::WaveUp(w),
        Op::ArmDown(d) => Tagged::ArmDown(d),
        Op::Died(d) => Tagged::Died(d),
        Op::InputAck { seq } => Tagged::InputAck { seq: *seq },
        Op::NoSeat { seats } => Tagged::NoSeat { seats: *seats },
        Op::Refused {
          measured_one_way_ms,
          allowed_one_way_ms,
        } => Tagged::Refused {
          measured_one_way_ms: *measured_one_way_ms,
          allowed_one_way_ms: *allowed_one_way_ms,
        },
      }
    }
  }

  /// Bytes on the wire, as the session would encode them.
  pub fn bytes(ops: &[Op]) -> usize {
    rmp_serde::to_vec(&ops).map(|v| v.len()).unwrap_or(0)
  }

  /// The same ops with numeric variant tags.
  pub fn bytes_numerically_tagged(ops: &[Op]) -> usize {
    let tagged: Vec<Tagged<'_>> = ops.iter().map(Tagged::from).collect();
    rmp_serde::to_vec(&tagged).map(|v| v.len()).unwrap_or(0)
  }

  /// Which of the two halves an op belongs to.
  ///
  /// A wave and an emitter death produce the entire enemy curtain. Everything
  /// else describes things a human caused, which cannot be derived.
  pub fn is_derivable_half(op: &Op) -> bool {
    matches!(op, Op::WaveUp(_) | Op::ArmDown(_))
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::sim::curtain::make_wave;
  use crate::sim::types::V2;

  fn frame_op(bullets: usize) -> Op {
    Op::Frame(Box::new(Frame {
      server_time_ms: 12_345,
      tick: 771,
      ships: vec![Ship::spawn(0), Ship::spawn(1)],
      bullets: (0..bullets as u32)
        .map(|id| PlayerBullet {
          id,
          owner: 0,
          pos: V2::new(id as f32, 40.0),
        })
        .collect(),
    }))
  }

  #[test]
  fn every_upstream_op_is_named_as_one() {
    for op in [
      Op::Move { seq: 1, tick: 1, dir: Dir8::N },
      Op::Fire { seq: 1, tick: 1 },
      Op::Struck { seq: 1, tick: 1 },
    ] {
      assert!(op.is_upstream(), "{op:?}");
    }
    for op in [Op::InputAck { seq: 1 }, Op::NoSeat { seats: 4 }] {
      assert!(!op.is_upstream(), "{op:?}");
    }
  }

  #[test]
  fn a_wave_buys_more_curtain_than_a_frame_buys_player_bullets() {
    // A wave is a fixed cost that becomes hundreds of bullets; player fire
    // costs bytes for every bullet.
    let wave = Op::WaveUp(Box::new(make_wave(0, 9_001, 0)));
    let wave_bytes = wire_cost::bytes(std::slice::from_ref(&wave));

    let mut bullets = Vec::new();
    crate::sim::curtain::curtain_at(&[crate::sim::curtain::make_wave(0, 9_001, 0)], &[], 400, &mut bullets);

    let streamed = frame_op(bullets.len());
    let streamed_bytes = wire_cost::bytes(std::slice::from_ref(&streamed));

    assert!(!bullets.is_empty());
    assert!(
      wave_bytes * 10 < streamed_bytes,
      "{} bullets: {wave_bytes} bytes derived against {streamed_bytes} streamed",
      bullets.len()
    );
  }

  #[test]
  fn compact_msgpack_still_spells_out_every_variant_name() {
    // `MsgPackCodec`'s docs say compact MessagePack keeps variant names, but
    // not what share of a frame they take. A stream of tiny messages is mostly
    // tag, so this is where the share is largest.
    let ops = vec![
      Op::InputAck { seq: 12 },
      Op::ArmDown(crate::sim::curtain::Downed { wave: 1, arm: 2, tick: 900 }),
      Op::InputAck { seq: 13 },
      Op::ArmDown(crate::sim::curtain::Downed { wave: 1, arm: 3, tick: 901 }),
    ];
    let named = wire_cost::bytes(&ops);
    let numeric = wire_cost::bytes_numerically_tagged(&ops);
    assert!(numeric < named, "numeric tags saved nothing: {named} against {numeric}");
    let share = (named - numeric) as f32 / named as f32;
    assert!(share > 0.15, "only {:.0}% of the frame was variant names", share * 100.0);
  }

  #[test]
  fn the_share_that_is_variant_names_falls_as_a_message_grows() {
    // This is why it stays a measurement and not a rule: a tag is a fixed cost,
    // so it dominates a stream of small events and is negligible in a large
    // frame.
    let small = vec![Op::InputAck { seq: 3 }];
    let large = vec![frame_op(200)];
    let small_share = (wire_cost::bytes(&small) - wire_cost::bytes_numerically_tagged(&small)) as f32 / wire_cost::bytes(&small) as f32;
    let large_share = (wire_cost::bytes(&large) - wire_cost::bytes_numerically_tagged(&large)) as f32 / wire_cost::bytes(&large) as f32;
    assert!(small_share > large_share * 10.0, "small {small_share:.3} large {large_share:.3}");
  }

  #[test]
  fn the_derivable_half_is_named_exhaustively() {
    // A new op that describes the curtain and is not listed here would be
    // counted against player fire and the main comparison would be skewed in
    // favour of the derived half.
    assert!(wire_cost::is_derivable_half(&Op::WaveUp(Box::new(make_wave(0, 1, 0)))));
    assert!(wire_cost::is_derivable_half(&Op::ArmDown(crate::sim::curtain::Downed { wave: 0, arm: 0, tick: 0 })));
    assert!(!wire_cost::is_derivable_half(&frame_op(1)));
    assert!(!wire_cost::is_derivable_half(&Op::InputAck { seq: 1 }));
  }
}
