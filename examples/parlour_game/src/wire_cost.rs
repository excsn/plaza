//! What the field names cost, on the traffic this game actually sends.
//!
//! The figure quoted for `MsgPackNamedCodec` (67% of JSON, against compact's
//! 40%) comes from a synthetic ten-op message. This repository has been misled
//! twice by that kind of number: a 55% win on despawns that was 0.7% of
//! traffic and a 15% variant-tag share that was 1%. A share depends on the
//! traffic mix, so it has to be measured on real traffic.
//!
//! What is measured here is the whole outbound stream of one match: every
//! broadcast notice, every refusal and the per-recipient snapshot the
//! controller builds for each seated player. The snapshot is by far the largest
//! message and dominates the total.
//!
//! # What it found
//!
//! Named costs +190% over compact on this traffic rather than the +67% the
//! synthetic figure implies. It lands at 76% of JSON where compact is 26%, so
//! picking named to keep a hand-written client simple gives up most of what
//! MessagePack saves.
//!
//! The reason generalises and is the opposite of what was expected. A field
//! name is paid per field per message, so the premium grows with a message's
//! field count rather than its size. `PlayerView` has sixteen fields and is
//! sent once per recipient per change; a notice has two or three behind a
//! variant name both encodings pay for. The widest, most frequent message
//! therefore pays most.
//!
//! This runs the other way from `curtain_fire`'s variant-name result, where a
//! fixed per-message tag made small messages the expensive ones.

use std::sync::atomic::AtomicU32;
use std::sync::Arc;

use plaza::agent::Agent;
use plaza::snapshot::SnapshotProvider;
use plaza::game_common::flow_control::TurnManager;
use plaza::state_logic::{LogicInput, StateLogic};
use plaza_wire::{MsgPackCodec, MsgPackNamedCodec, WireCodec};
use serde::Serialize;

use crate::snapshot::TableSnapshotter;
use crate::table::TableLogic;
use crate::types::{PlayerId, TableOp, TablePhase, TableSettings, TableState};
use crate::wallets::WalletRegistry;

/// Bytes one stream costs under each encoding.
#[derive(Debug, Clone, Copy, Default)]
pub struct Cost {
  pub json: usize,
  pub compact: usize,
  pub named: usize,
  pub messages: usize,
}

impl Cost {
  fn add<T: Serialize>(&mut self, value: &T) {
    self.json += serde_json::to_vec(value).expect("json").len();
    self.compact += MsgPackCodec.encode(value).expect("compact").len();
    self.named += MsgPackNamedCodec.encode(value).expect("named").len();
    self.messages += 1;
  }

  /// What named costs over compact, as a share of compact.
  pub fn names_premium(&self) -> f64 {
    (self.named as f64 - self.compact as f64) / self.compact as f64
  }

  pub fn compact_of_json(&self) -> f64 {
    self.compact as f64 / self.json as f64
  }

  pub fn named_of_json(&self) -> f64 {
    self.named as f64 / self.json as f64
  }
}

/// The outbound stream of one full match, split into the two parts that behave
/// differently.
#[derive(Debug, Clone, Copy, Default)]
pub struct MatchCost {
  /// Broadcasts and refusals: small messages, mostly variant name.
  pub notices: Cost,
  /// One per recipient per resnapshot. Large, and the bulk of the wire.
  pub snapshots: Cost,
}

impl MatchCost {
  pub fn total(&self) -> Cost {
    Cost {
      json: self.notices.json + self.snapshots.json,
      compact: self.notices.compact + self.snapshots.compact,
      named: self.notices.named + self.snapshots.named,
      messages: self.notices.messages + self.snapshots.messages,
    }
  }
}

/// Plays one match to its end, costing everything it puts on the wire.
///
/// Deterministic, so the number does not move between runs. The deal is
/// shuffled from a seed of the table's name, the tick and the deal count, all
/// of which are the same on every run. Every play is the on-turn player's
/// lowest card.
pub async fn measure_a_match(seats: u32) -> MatchCost {
  let mut cost = MatchCost::default();
  let mut state = TableState::new(
    "measured".into(),
    TableSettings {
      stake: 10,
      turn_timeout_ticks: 1_000,
      budget_ms: None,
    },
    seats,
    Arc::new(WalletRegistry::new()),
    Arc::new(AtomicU32::new(0)),
  );

  let players: Vec<PlayerId> = (1..=seats as PlayerId).collect();

  let record = |cost: &mut MatchCost, output: &plaza::state_logic::LogicOutput<TableOp, PlayerId>| {
    for targeted in &output.ops {
      for op in &targeted.ops {
        cost.notices.add(op);
      }
    }
  };

  for id in &players {
    let out = TableLogic
      .process_input(&mut state, LogicInput::AgentOps {
        source: Agent::system(),
        ops: vec![TableOp::Reserve { player: *id }],
      })
      .await
      .expect("reserve");
    record(&mut cost, &out);

    let out = TableLogic
      .process_input(&mut state, LogicInput::AgentJoined {
        agent: Agent::new_human(*id),
      })
      .await
      .expect("join");
    record(&mut cost, &out);
    cost.snapshots = snapshot_pass(&state, &players, cost.snapshots).await;
  }

  while *state.phase.current() != TablePhase::Finished {
    let Some(on_turn) = state.turns.current_turn_actor() else { break };
    let Some(card) = state.hands.get(&on_turn).and_then(|hand| hand.iter().min().copied()) else {
      break;
    };
    let out = TableLogic
      .process_input(&mut state, LogicInput::AgentOps {
        source: Agent::new_human(on_turn),
        ops: vec![TableOp::PlayCard(card)],
      })
      .await
      .expect("play");
    record(&mut cost, &out);
    if !out.snapshots.is_empty() {
      cost.snapshots = snapshot_pass(&state, &players, cost.snapshots).await;
    }
  }

  cost
}

/// One snapshot per recipient, which is what the controller does.
async fn snapshot_pass(state: &TableState, players: &[PlayerId], mut into: Cost) -> Cost {
  for id in players {
    let op = TableSnapshotter
      .create_snapshot(state, Some(&Agent::new_human(*id)), None)
      .await
      .expect("snapshot");
    if let Some(op) = op {
      into.add(&op);
    }
  }
  into
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Named nearly triples this wire and lands close to JSON.
  ///
  /// Bounded rather than exact, because the deal and the rules decide the mix
  /// and a rules change should not fail this. The bounds hold the conclusion:
  /// compact is roughly a quarter of JSON and named roughly three quarters.
  #[tokio::test]
  async fn the_names_nearly_triple_a_real_match() {
    let cost = measure_a_match(3).await;
    let total = cost.total();

    assert!(total.messages > 20, "too little traffic to conclude anything");
    assert!(
      total.names_premium() > 1.0,
      "the premium collapsed to {:.0}%; the synthetic 67%-of-JSON figure would \
       have been right after all, and this module's reason for existing is gone",
      total.names_premium() * 100.0
    );
    assert!(
      total.compact_of_json() < 0.4,
      "compact is {:.0}% of json, worse than the 40% the crate docs claim",
      total.compact_of_json() * 100.0
    );
    assert!(
      total.named_of_json() > 0.6,
      "named came out at {:.0}% of json, better than the 67% the crate docs claim",
      total.named_of_json() * 100.0
    );
  }

  /// Wide snapshots pay a larger premium than narrow notices, which is the
  /// opposite of the initial guess.
  ///
  /// A field name is paid per field per message, so the premium grows with how
  /// many fields a message has rather than how small it is. A notice is two or
  /// three short fields behind a variant name both encodings pay for; a
  /// per-recipient view is sixteen. So the largest and most frequent message on
  /// this wire also pays proportionally most. In `curtain_fire` a fixed
  /// per-message tag made small messages the expensive ones instead.
  #[tokio::test]
  async fn a_wide_snapshot_pays_more_than_a_narrow_notice() {
    let cost = measure_a_match(3).await;

    assert!(
      cost.snapshots.names_premium() > cost.notices.names_premium(),
      "snapshots {:.0}% against notices {:.0}%: the premium stopped tracking field count",
      cost.snapshots.names_premium() * 100.0,
      cost.notices.names_premium() * 100.0
    );
    assert!(
      cost.snapshots.compact > cost.notices.compact,
      "snapshots stopped dominating the wire, so the total is no longer their premium"
    );
  }
}
