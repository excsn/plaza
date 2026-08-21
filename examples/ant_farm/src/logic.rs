//! The tick: step, bucket, publish, deal. Every phase is timed separately,
//! because which one owns the frame is the finding.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::error::StateLogicError;
use plaza::session::{MessageTarget, Session, SessionMessage, TargetedOp};
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic};
use plaza::stats::ControllerStats;
use plaza_server_utils::oneshot::Pending;

use crate::panel::{print_line, Panel, WireStats};
use crate::protocol::{AntOp, Packed, WatcherId, CELL, TICK_HZ};
use crate::publish::{assemble, assemble_counts, pane_cells, Buckets, Publication, Wanted};
use crate::sim::Colony;

pub const STEP_MS: u64 = 1000 / TICK_HZ;

/// A pane a watcher asked for, with its cell list cached until it moves.
struct Watcher {
  cells: Vec<usize>,
  coarse: bool,
}

pub struct FarmState {
  pub colony: Colony,
  buckets: Buckets,
  publication: Publication,
  wanted: Wanted,
  watchers: HashMap<WatcherId, Watcher>,
  pending: Pending<WatcherId, AntOp>,
  panel: Panel,
  wire: Arc<WireStats>,
  controller: Arc<ControllerStats>,
  pub tick: u32,
}

impl std::fmt::Debug for FarmState {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("FarmState")
      .field("ants", &self.colony.len())
      .field("watchers", &self.watchers.len())
      .field("tick", &self.tick)
      .finish_non_exhaustive()
  }
}

impl FarmState {
  pub fn new(colony: Colony, wire: Arc<WireStats>, controller: Arc<ControllerStats>) -> Self {
    let space = colony.space().clone();
    Self {
      buckets: Buckets::new(space.clone()),
      publication: Publication::new(space.clone()),
      wanted: Wanted::new(&space),
      watchers: HashMap::new(),
      pending: Pending::new(),
      panel: Panel::new(),
      wire,
      controller,
      tick: 0,
      colony,
    }
  }

  fn welcome(&self) -> AntOp {
    AntOp::Welcome {
      tick: self.tick,
      extent: self.colony.extent(),
      cell: CELL,
      population: self.colony.len() as u32,
      nest: self.colony.nest,
      sites: self.colony.sites.clone(),
    }
  }

  fn now_ms(&self) -> u64 {
    self.tick as u64 * STEP_MS
  }
}

/// Holds the session itself for the `Cells` fan-out: the controller
/// coalesces neighbouring same-target ops into one envelope, which is right
/// on a stream and fatal on a datagram link where every frame must fit the
/// MTU. Sending one message per payload keeps one payload per datagram.
pub struct AntLogic {
  session: Arc<dyn Session<AntOp, WatcherId>>,
}

impl AntLogic {
  pub fn new(session: Arc<dyn Session<AntOp, WatcherId>>) -> Self {
    Self { session }
  }
}

#[async_trait]
impl StateLogic<AntOp, WatcherId, FarmState> for AntLogic {
  async fn process_input(
    &self,
    state: &mut FarmState,
    input: LogicInput<AntOp, WatcherId>,
  ) -> Result<LogicOutput<AntOp, WatcherId>, StateLogicError> {
    match input {
      LogicInput::AgentJoined { agent } => {
        let Some(id) = agent.id_cloned() else {
          return Ok(LogicOutput::default());
        };
        state.watchers.entry(id).or_insert(Watcher {
          cells: Vec::new(),
          coarse: false,
        });
        let now = state.now_ms();
        let welcome = state.pending.declare(id, state.welcome(), now);
        Ok(vec![TargetedOp::new_system_to(id, vec![welcome])].into())
      }

      LogicInput::AgentLeft { agent_id } => {
        state.watchers.remove(&agent_id);
        state.pending.confirm(&agent_id);
        Ok(LogicOutput::default())
      }

      LogicInput::AgentOps { source, ops } => {
        let Some(id) = source.id_cloned() else {
          return Ok(LogicOutput::default());
        };
        for op in ops {
          match op {
            AntOp::Window { x, y, half, coarse } => {
              let cells = pane_cells(state.colony.space(), x, y, half.clamp(CELL, state.colony.extent()));
              state.watchers.insert(id, Watcher { cells, coarse });
            }
            AntOp::WelcomeSeen => state.pending.confirm(&id),
            AntOp::Dial { ants } => {
              if ants > 0 {
                state.colony.resize(ants as usize);
              }
            }
            _ => {}
          }
        }
        Ok(LogicOutput::default())
      }

      LogicInput::TimeStep { delta_time } => {
        let dt = delta_time.as_secs_f32();
        state.tick = state.tick.wrapping_add(1);

        let begun = Instant::now();
        state.colony.step(dt);
        let stepped = Instant::now();
        state.panel.step.record((stepped - begun).as_nanos() as u64);

        state.buckets.rebuild(&state.colony);
        let bucketed = Instant::now();
        state.panel.rebuild.record((bucketed - stepped).as_nanos() as u64);

        state.wanted.reset();
        for watcher in state.watchers.values() {
          if !watcher.coarse {
            state.wanted.mark(&watcher.cells);
          }
        }
        state.publication.publish(&state.colony, &state.buckets, &state.wanted);
        let published = Instant::now();
        state.panel.publish.record((published - bucketed).as_nanos() as u64);

        let mut payloads = Vec::new();
        for (&id, watcher) in &state.watchers {
          payloads.clear();
          if watcher.coarse {
            assemble_counts(&state.buckets, &watcher.cells, &mut payloads);
          } else {
            assemble(&state.publication, &watcher.cells, &mut payloads);
          }
          for payload in payloads.drain(..) {
            let bytes = Packed::new(payload);
            let op = if watcher.coarse {
              AntOp::Counts { tick: state.tick, bytes }
            } else {
              AntOp::Cells { tick: state.tick, bytes }
            };
            let msg = SessionMessage::new(Agent::system(), vec![op]);
            if let Err(e) = self.session.send_message(MessageTarget::Agent(id), msg).await {
              tracing::debug!(watcher = id, error = %e, "pane payload not sent.");
            }
          }
        }
        let dealt = Instant::now();
        state.panel.assemble.record((dealt - published).as_nanos() as u64);

        let mut ops: Vec<TargetedOp<AntOp, WatcherId>> = Vec::new();
        for (id, op) in state.pending.due(state.now_ms(), true) {
          if state.watchers.contains_key(&id) {
            ops.push(TargetedOp::new_system_to(id, vec![op]));
          }
        }

        if let Some(mut snapshot) = state.panel.tick(dt as f64 * 1000.0, &state.wire) {
          snapshot.ants = state.colony.len() as u32;
          snapshot.watchers = state.watchers.len() as u32;
          snapshot.packed_cells = state.publication.packed_cells as u32;
          snapshot.delivered = state.colony.delivered;
          snapshot.tick_mean_ms = state.controller.mean_tick().as_secs_f32() * 1000.0;
          snapshot.tick_worst_ms = state.controller.worst_tick().as_secs_f32() * 1000.0;
          print_line(&snapshot);
          ops.push(TargetedOp::new_system_all(vec![AntOp::Stats(snapshot)]));
        }

        Ok(ops.into())
      }
    }
  }
}
