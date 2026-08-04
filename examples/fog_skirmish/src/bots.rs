//! Scouts driven from a player's own view of the world.
//!
//! They run in the server process and could read `FogState`, which would let
//! them walk straight to an uncaptured relic across the map. They read
//! [`player_view`] through `query_with` instead: a bot that plays on
//! information the fog is supposed to deny would make the example prove the
//! opposite of what it claims.
//!
//! So a bot here is genuinely exploring. It heads for the nearest relic it can
//! actually see, and when it can see none, it sweeps to a corner it has not
//! visited lately, which is what a player does.

use std::time::Duration;

use plaza::{
  agent::Agent,
  controller::{query_with, CommandSender, ControllerCommand},
};
use tracing::info;

use crate::snapshot::player_view;
use crate::types::{FogOp, FogState, PlayerId, PlayerView, FIELD};

pub type FogCommands = CommandSender<FogOp, PlayerId, FogState>;

const THINK: Duration = Duration::from_millis(400);

/// Corners a bot sweeps between when it has nothing in sight.
const SWEEP: [(f32, f32); 5] = [
  (FIELD * 0.5, FIELD * 0.5),
  (FIELD * 0.2, FIELD * 0.8),
  (FIELD * 0.8, FIELD * 0.2),
  (FIELD * 0.8, FIELD * 0.8),
  (FIELD * 0.2, FIELD * 0.2),
];

/// Where this bot heads next, given only what it was sent.
fn choose(view: &PlayerView, sweep: usize) -> (f32, f32) {
  let me = view.you;
  let anchor = view.my_units.first();

  // The nearest relic in sight that is not already ours.
  let target = view
    .relics
    .iter()
    .filter(|r| r.owner != Some(me))
    .min_by(|a, b| {
      let (ax, ay) = anchor.map_or((0.0, 0.0), |u| (u.x, u.y));
      let da = (a.x - ax).powi(2) + (a.y - ay).powi(2);
      let db = (b.x - ax).powi(2) + (b.y - ay).powi(2);
      da.total_cmp(&db)
    });

  match target {
    Some(relic) => (relic.x, relic.y),
    None => SWEEP[sweep % SWEEP.len()],
  }
}

pub async fn spawn_bots(tx: FogCommands, ids: Vec<PlayerId>) {
  for id in &ids {
    if tx
      .send(ControllerCommand::HandleAgentJoined {
        agent: Agent::new_bot(*id),
      })
      .await
      .is_err()
    {
      return;
    }
  }
  info!(bots = ids.len(), "bot scouts deployed");

  let mut ticker = tokio::time::interval(THINK);
  let mut sweep = 0usize;
  loop {
    ticker.tick().await;
    sweep += 1;
    for (nth, id) in ids.iter().enumerate() {
      let player = *id;
      let Ok(view) = query_with(&tx, move |state: &FogState| player_view(state, player)).await else {
        return;
      };
      if view.my_units.is_empty() {
        continue;
      }
      let (x, y) = choose(&view, sweep + nth);
      if tx
        .send(ControllerCommand::SubmitAgentOps {
          agent: Agent::new_bot(player),
          ops: vec![FogOp::MoveTo { x, y }],
        })
        .await
        .is_err()
      {
        return;
      }
    }
  }
}
