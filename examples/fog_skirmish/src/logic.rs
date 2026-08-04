use async_trait::async_trait;
use plaza::{
  agent::Agent,
  session::TargetedOp,
  state_logic::{LogicInput, LogicOutput, SnapshotRequest, StateLogic, StateLogicError},
};
use tracing::{debug, info};

use crate::types::{
  FogOp, FogState, Player, PlayerId, PlayerStats, Unit, Withheld, CAPTURE_RADIUS, CAPTURE_TICKS, FIELD,
  SCOUTS_PER_PLAYER, SCOUT_SPEED,
};
use crate::vision::{can_see, leaks_in};

#[derive(Debug, Default)]
pub struct FogLogic;

#[async_trait]
impl StateLogic<FogOp, PlayerId, FogState> for FogLogic {
  async fn process_input(
    &self,
    state: &mut FogState,
    input: LogicInput<FogOp, PlayerId>,
  ) -> Result<LogicOutput<FogOp, PlayerId>, StateLogicError> {
    let mut out: Vec<TargetedOp<FogOp, PlayerId>> = Vec::new();

    match input {
      LogicInput::AgentOps { source, ops } => {
        let Some(player) = source.id_cloned() else {
          return Ok(LogicOutput::none());
        };
        for op in ops {
          match op {
            FogOp::MoveTo { x, y } => {
              let (x, y) = (x.clamp(0.0, FIELD), y.clamp(0.0, FIELD));
              // The lead scout stands *on* the point, because any spread wide
              // enough to be worth having is wider than `CAPTURE_RADIUS`, and a
              // squad that fans out around a relic never takes it. The other two
              // fan out for vision, which is what three scouts are for.
              for (nth, unit) in state.units.iter_mut().filter(|u| u.owner == player).enumerate() {
                let (dx, dy) = if nth == 0 {
                  (0.0, 0.0)
                } else {
                  let angle = nth as f32 * 2.094;
                  (angle.cos() * 11.0, angle.sin() * 11.0)
                };
                unit.to = Some(((x + dx).clamp(0.0, FIELD), (y + dy).clamp(0.0, FIELD)));
              }
            }
            FogOp::SetLeakMode(on) => {
              state.leak_mode = on;
              info!(%player, leaking = on, "leak mode toggled");
              if on {
                // Everything held back is told at once, which is what an
                // implementation without the deferral would have done all
                // along. The counter moves the instant it is switched on.
                let backlog: Vec<(PlayerId, Vec<Withheld>)> = state
                  .players
                  .iter_mut()
                  .map(|(id, p)| (*id, std::mem::take(&mut p.withheld)))
                  .collect();
                for (recipient, events) in backlog {
                  for held in events {
                    out.push(TargetedOp::new_system_to(
                      recipient,
                      vec![FogOp::Captured {
                        relic: held.relic,
                        x: held.x,
                        y: held.y,
                        by: held.by,
                        tick: held.tick,
                        late: true,
                      }],
                    ));
                  }
                }
              }
            }
            FogOp::Welcome { .. } | FogOp::Snapshot(_) | FogOp::Captured { .. } => {
              debug!(%player, "ignoring a server-originated op from a client");
            }
          }
        }
        Ok(audited(state, out))
      }

      LogicInput::AgentJoined { agent } => {
        let Some(player) = agent.id_cloned() else {
          return Ok(LogicOutput::none());
        };
        if state.players.contains_key(&player) {
          return Ok(LogicOutput::none());
        }

        // A corner each, so nobody starts inside anyone else's vision.
        let corner = state.players.len();
        let (cx, cy) = [
          (FIELD * 0.15, FIELD * 0.15),
          (FIELD * 0.85, FIELD * 0.85),
          (FIELD * 0.85, FIELD * 0.15),
          (FIELD * 0.15, FIELD * 0.85),
        ][corner % 4];

        for n in 0..SCOUTS_PER_PLAYER {
          let angle = n as f32 * 2.094;
          state.units.push(Unit {
            id: state.next_unit,
            owner: player,
            x: (cx + angle.cos() * 6.0).clamp(0.0, FIELD),
            y: (cy + angle.sin() * 6.0).clamp(0.0, FIELD),
            to: None,
          });
          state.next_unit += 1;
        }

        state.players.insert(
          player,
          Player {
            bot: matches!(agent, Agent::Bot(_)),
            agent,
            score: 0,
            withheld: Vec::new(),
            stats: PlayerStats::default(),
          },
        );
        info!(%player, players = state.players.len(), "scouts deployed");

        Ok(LogicOutput::ops(vec![TargetedOp::new_system_to(
          player,
          vec![FogOp::Welcome { you: player }],
        )]))
      }

      LogicInput::AgentLeft { agent_id } => {
        state.players.remove(&agent_id);
        state.units.retain(|u| u.owner != agent_id);
        for relic in state.relics.iter_mut() {
          if relic.claimant == Some(agent_id) {
            relic.claimant = None;
            relic.progress = 0;
          }
        }
        info!(%agent_id, "player left, scouts withdrawn");
        Ok(LogicOutput::none())
      }

      LogicInput::TimeStep { delta_time } => {
        state.tick += 1;
        let dt = delta_time.as_secs_f32();

        for unit in state.units.iter_mut() {
          let Some((tx, ty)) = unit.to else { continue };
          let (dx, dy) = (tx - unit.x, ty - unit.y);
          let gap = (dx * dx + dy * dy).sqrt();
          let step = SCOUT_SPEED * dt;
          if gap <= step {
            unit.x = tx;
            unit.y = ty;
            unit.to = None;
          } else {
            unit.x += dx / gap * step;
            unit.y += dy / gap * step;
          }
        }

        capture(state, &mut out);
        release_withheld(state, &mut out);

        if state.players.is_empty() {
          return Ok(audited(state, out));
        }
        // Per recipient, deliberately, and the opposite of `tag_arena`: the
        // whole point is that two players are sent different worlds.
        let everyone = state.players.values().map(|p| p.agent.clone()).collect();
        Ok(audited(state, out).and_snapshot(SnapshotRequest::to(everyone)))
      }
    }
  }
}

/// Advances claims, and tells whoever is allowed to know.
fn capture(state: &mut FogState, out: &mut Vec<TargetedOp<FogOp, PlayerId>>) {
  let mut captured: Vec<(u32, f32, f32, PlayerId)> = Vec::new();

  for relic in state.relics.iter_mut() {
    // Whoever is standing on it, if exactly one player is.
    let mut claimant = None;
    let mut contested = false;
    for unit in state.units.iter() {
      let (dx, dy) = (unit.x - relic.x, unit.y - relic.y);
      if dx * dx + dy * dy > CAPTURE_RADIUS * CAPTURE_RADIUS {
        continue;
      }
      match claimant {
        None => claimant = Some(unit.owner),
        Some(other) if other != unit.owner => contested = true,
        Some(_) => {}
      }
    }

    if contested || claimant.is_none() || claimant == relic.owner {
      relic.claimant = None;
      relic.progress = 0;
      continue;
    }

    let claimant = claimant.expect("checked above");
    if relic.claimant == Some(claimant) {
      relic.progress += 1;
    } else {
      relic.claimant = Some(claimant);
      relic.progress = 1;
    }

    if relic.progress >= CAPTURE_TICKS {
      relic.owner = Some(claimant);
      relic.claimant = None;
      relic.progress = 0;
      captured.push((relic.id, relic.x, relic.y, claimant));
    }
  }

  for (id, x, y, by) in captured {
    if let Some(player) = state.players.get_mut(&by) {
      player.score += 1;
    }
    info!(relic = id, %by, tick = state.tick, "relic captured");

    let watchers: Vec<PlayerId> = state.players.keys().copied().collect();
    for recipient in watchers {
      // The capturing player is standing on it, so this is never withheld from
      // them: the rule is about what you can see, not about who you are.
      let visible = can_see(state, recipient, x, y);
      if visible || state.leak_mode {
        out.push(TargetedOp::new_system_to(
          recipient,
          vec![FogOp::Captured {
            relic: id,
            x,
            y,
            by,
            tick: state.tick,
            late: false,
          }],
        ));
        if let Some(player) = state.players.get_mut(&recipient) {
          player.stats.told += 1;
        }
      } else if let Some(player) = state.players.get_mut(&recipient) {
        // Held whole. Telling them "something happened somewhere" would leak
        // the timing, and telling them nothing ever would leave two boards
        // disagreeing about a relic they both end up looking at.
        player.withheld.push(Withheld {
          relic: id,
          x,
          y,
          by,
          tick: state.tick,
        });
      }
    }
  }
}

/// Tells players what they were not allowed to hear, now that they can see it.
fn release_withheld(state: &mut FogState, out: &mut Vec<TargetedOp<FogOp, PlayerId>>) {
  let players: Vec<PlayerId> = state.players.keys().copied().collect();
  for recipient in players {
    let held = match state.players.get(&recipient) {
      Some(player) if !player.withheld.is_empty() => player.withheld.clone(),
      _ => continue,
    };

    let mut still_hidden = Vec::with_capacity(held.len());
    let mut releasing = Vec::new();
    for event in held {
      if can_see(state, recipient, event.x, event.y) {
        releasing.push(event);
      } else {
        still_hidden.push(event);
      }
    }

    if let Some(player) = state.players.get_mut(&recipient) {
      player.withheld = still_hidden;
      player.stats.told_late += releasing.len() as u64;
    }
    for event in releasing {
      out.push(TargetedOp::new_system_to(
        recipient,
        vec![FogOp::Captured {
          relic: event.relic,
          x: event.x,
          y: event.y,
          by: event.by,
          tick: event.tick,
          late: true,
        }],
      ));
    }
  }
}

/// Counts, on the way out, every position this batch tells someone about that
/// they could not see.
///
/// Not a guard: nothing here drops an op. An example that quietly repaired its
/// own leaks would have a panel reading zero for two different reasons, and the
/// number is only worth watching if it is free to move.
fn audited(state: &mut FogState, out: Vec<TargetedOp<FogOp, PlayerId>>) -> LogicOutput<FogOp, PlayerId> {
  let mut counts: Vec<(PlayerId, u64)> = Vec::new();
  for targeted in &out {
    for recipient in recipients_of(state, targeted) {
      let leaked: usize = targeted.ops.iter().map(|op| leaks_in(state, recipient, op)).sum();
      if leaked > 0 {
        counts.push((recipient, leaked as u64));
      }
    }
  }
  for (recipient, leaked) in counts {
    if let Some(player) = state.players.get_mut(&recipient) {
      player.stats.leaks += leaked;
    }
  }
  LogicOutput::ops(out)
}

fn recipients_of(state: &FogState, targeted: &TargetedOp<FogOp, PlayerId>) -> Vec<PlayerId> {
  use plaza::session::MessageTarget;
  match &targeted.target {
    MessageTarget::Agent(id) => vec![*id],
    MessageTarget::Agents(ids) => ids.clone(),
    MessageTarget::All => state.players.keys().copied().collect(),
    MessageTarget::AllExcept(id) => state.players.keys().copied().filter(|p| p != id).collect(),
    MessageTarget::AllExceptThese(ids) => state.players.keys().copied().filter(|p| !ids.contains(p)).collect(),
  }
}
