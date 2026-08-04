//! Bots that play from the world view, not from the state.
//!
//! They live in the server process, so they could read `ArenaState` directly.
//! They read `world_view` through `query_with` instead, which is the same
//! `WorldSnapshot` a browser receives: a bot with privileged information is not
//! playing the game it appears to be playing, and its behaviour stops being
//! evidence that a real client could do the same.

use std::time::Duration;

use plaza::{
  agent::Agent,
  controller::{query_with, CommandSender, ControllerCommand},
};
use tracing::info;

use crate::{
  snapshot::world_view,
  types::{ArenaOp, ArenaState, PlayerId, WorldSnapshot, FIELD},
};

pub type ArenaCommands = CommandSender<ArenaOp, PlayerId, ArenaState>;

/// How often a bot reconsiders. Well under the 60Hz tick: a runner that
/// re-aims 20 times a second is already a harder target than most humans.
const THINK: Duration = Duration::from_millis(50);

/// How near a wall a fleeing runner starts turning away from it.
const WALL_MARGIN: f32 = FIELD * 0.3;
/// How close "it" has to be before a runner bothers fleeing.
const THREAT: f32 = FIELD * 0.35;

/// Where `me` wants to go, given the world as everyone can see it.
///
/// While "it": toward the nearest taggable runner. Otherwise: away from "it",
/// turning off the walls before reaching them. Fleeing straight away from a
/// chaser ends in a corner every time, which is a stationary target.
pub fn steer_for(me: PlayerId, world: &WorldSnapshot) -> Option<(f32, f32)> {
  let my = world.runners.iter().find(|r| r.id == me)?;
  match world.it {
    Some(it_id) if it_id == me => world
      .runners
      .iter()
      .filter(|r| r.id != me && Some(r.id) != world.no_tag_back && r.in_play)
      .min_by(|a, b| {
        let da = (a.x - my.x).powi(2) + (a.y - my.y).powi(2);
        let db = (b.x - my.x).powi(2) + (b.y - my.y).powi(2);
        da.total_cmp(&db)
      })
      .map(|prey| (prey.x - my.x, prey.y - my.y)),
    Some(it_id) => world.runners.iter().find(|r| r.id == it_id).map(|it| {
      let (mut fx, mut fy) = (my.x - it.x, my.y - it.y);
      let away = (fx * fx + fy * fy).sqrt();
      // Nothing to run from yet. Without this a runner that escaped once keeps
      // fleeing a chaser on the far side of the field, and spends the rest of
      // the game in the corner that escape ended in.
      if away > THREAT {
        return (FIELD / 2.0 - my.x, FIELD / 2.0 - my.y);
      }
      if away > f32::EPSILON {
        fx /= away;
        fy /= away;
      }
      // A wall term that reaches 1.0 at the wall itself, so the closer edge
      // outvotes the chaser before there is nowhere left to go.
      let off = |gap: f32| (WALL_MARGIN - gap).max(0.0) / WALL_MARGIN;
      let (wx, wy) = (
        off(my.x) - off(FIELD - my.x),
        off(my.y) - off(FIELD - my.y),
      );
      // Turn *along* the wall rather than into it: running straight away from a
      // chaser and pushing back off the wall cancel out, which parks a runner
      // in the corner it was trying to escape. The perpendicular that agrees
      // with the wall is the way around it.
      let (mut tx, mut ty) = (-fy, fx);
      if tx * wx + ty * wy < 0.0 {
        tx = -tx;
        ty = -ty;
      }
      let crowding = (wx * wx + wy * wy).sqrt();
      (fx + wx + tx * crowding * 1.5, fy + wy + ty * crowding * 1.5)
    }),
    None => None,
  }
}

/// Seats `count` bots and steers them until the controller stops.
pub async fn spawn_bots(tx: ArenaCommands, ids: Vec<PlayerId>) {
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
  info!(bots = ids.len(), "Bots seated.");

  let mut ticker = tokio::time::interval(THINK);
  loop {
    ticker.tick().await;
    let Ok(world) = query_with(&tx, world_view).await else {
      return;
    };
    for id in &ids {
      let Some((dx, dy)) = steer_for(*id, &world) else {
        continue;
      };
      let sent = tx
        .send(ControllerCommand::SubmitAgentOps {
          agent: Agent::new_bot(*id),
          ops: vec![ArenaOp::Steer { dx, dy }],
        })
        .await;
      if sent.is_err() {
        return;
      }
    }
  }
}
