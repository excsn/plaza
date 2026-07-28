//! The authoritative arena, as `plaza` core wants it: one `StateType` that owns
//! everything mutable, and one stateless `StateLogic` that acts on it.
//!
//! The adaptation is small, because [`sim::Server`] was already shaped for it:
//! it never reads client state, `advance` is a tick function, and inputs are
//! already addressed by tick rather than applied on arrival. What this adds is
//! seats that fill and empty, and the impairment link that makes the host's
//! latency sliders act on a real outbound path rather than on a simulation of
//! one.
//!
//! [`sim::Server`]: crate::sim::Server

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use plaza::session::{MessageTarget, TargetedOp};
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic, StateLogicError};
use plaza::Agent;
use plaza_client_utils::net_sim::{LatencyLink, Rng};
use plaza_server_utils::{SeatTable, Seating};

use crate::sim::protocol::{Intent, Op, ServerPolicy, PROTOCOL};
use crate::sim::server::Server;
use crate::sim::types::{BombState, Cell, Controls, Grid, PlayerId, PlayerState, PowerupState};

/// How a connection is identified. Assigned by the server on accept, never
/// supplied by the client.
pub type PlayerKey = u64;

/// Fixed, so dragging the jitter slider gives the same distribution every run
/// rather than one that depends on when the process started.
const IMPAIR_SEED: u64 = 0x0B0B_1E55;

/// Everything the omniscient half of a host needs, published by the arena and
/// read by the host's UI and renderer.
///
/// A host is the server *and* a client in one process, so unlike a joiner it
/// legitimately holds both: the truth here, and its own believed state in its
/// [`NetClient`]. Drawing the two over each other is what makes a snap visible
/// as a thing that happened rather than a number in a panel.
///
/// [`NetClient`]: crate::net::client::NetClient
#[derive(Clone, Debug, Default)]
pub struct HostView {
  pub grid: Grid,
  pub players: Vec<PlayerState>,
  pub bombs: Vec<BombState>,
  pub powerups: Vec<PowerupState>,
  pub fire: Vec<Cell>,
  pub round: u32,
  pub server_now_ms: u64,

  pub kills: u64,
  pub walls_destroyed: u64,
  pub bombs_placed: u64,
  pub longest_chain: usize,
  /// `(accepted, late, closed, ahead, last margin)` per seat. The host-side
  /// half of a joiner's own input readout, and the only place a rejection is
  /// visible at all: an input is acknowledged on arrival, before admission, so
  /// a refused one looks exactly like an applied one from the client.
  pub input_verdicts: Vec<(u64, u64, u64, u64, Option<i64>)>,
  pub seats_taken: usize,
  pub seats: usize,
}

/// Frames held for `latency ± jitter` before release, so the host's sliders act
/// on the real outbound path.
///
/// [`LatencyLink`] defaults to ordered delivery, which is what a WebSocket
/// actually does and what this needs: [`Op::Blast`] is order-sensitive against
/// [`Op::Frame`], because a blast clears walls the next frame's positions are
/// predicated on.
type Downlink = LatencyLink<Op>;

/// Everything the arena owns. `plaza` requires `Clone` for its state-query
/// command; nothing on the hot path clones it.
#[derive(Clone, Debug)]
pub struct Arena {
  pub sim: Server,
  pub controls: Controls,
  seats: SeatTable<PlayerKey>,
  /// The newest input sequence accepted per player, echoed back so a client can
  /// bound its replay buffer.
  acked: HashMap<PlayerKey, u64>,
  down: HashMap<PlayerKey, Downlink>,
  rng: Rng,
}

impl Arena {
  pub fn new(controls: Controls, seed: u64) -> Self {
    let count = controls.players.clamp(1, 4);
    Self {
      sim: Server::new(count, seed),
      controls,
      seats: SeatTable::new(count),
      acked: HashMap::new(),
      down: HashMap::new(),
      rng: Rng::new(IMPAIR_SEED),
    }
  }

  pub fn policy(&self) -> ServerPolicy {
    ServerPolicy {
      sync_hz: self.controls.sync_hz,
      playout_delay_ms: self.controls.playout_delay_ms,
      render_delay_ms: self.controls.render_delay_ms,
      input_max_late_ticks: self.controls.input_max_late_ticks,
      input_max_early_ticks: self.controls.input_max_early_ticks,
      players: self.sim.seats(),
    }
  }

  pub fn seat_of(&self, key: &PlayerKey) -> Option<usize> {
    self.seats.seat_of(key)
  }

  /// Seats a joiner, or refuses when the arena is full. Refusing is a real
  /// outcome: a demo people can share is a demo people can overfill.
  fn seat(&mut self, key: PlayerKey) -> Option<usize> {
    let seating = self.seats.seat(key);
    if let Seating::Fresh(seat) = seating {
      self.sim.take_seat(seat);
    }
    seating.index()
  }

  fn unseat(&mut self, key: &PlayerKey) {
    if let Some(seat) = self.seats.unseat(key) {
      // Handed back to the bots rather than left frozen, so a disconnect does
      // not leave a statue standing in the arena soaking up blasts.
      self.sim.release_seat(seat);
    }
    self.acked.remove(key);
    self.down.remove(key);
  }

  fn host_view(&self) -> HostView {
    HostView {
      grid: self.sim.grid.clone(),
      players: self.sim.players.clone(),
      bombs: self.sim.bombs.clone(),
      powerups: self.sim.powerups.clone(),
      fire: self.sim.fire_cells(),
      round: self.sim.round(),
      server_now_ms: self.sim.now_ms(),
      kills: self.sim.kills,
      walls_destroyed: self.sim.walls_destroyed,
      bombs_placed: self.sim.bombs_placed,
      longest_chain: self.sim.longest_chain,
      input_verdicts: self.sim.input_verdicts(),
      seats_taken: self.seats.by_seat().len(),
      seats: self.sim.seats(),
    }
  }
}

/// The stateless half plaza acts through.
///
/// Carries two shared slots rather than owning that state: `controls` is
/// written by the host's panel and read here every tick, and `view` is written
/// here and read by the host's renderer. A headless server has neither, so its
/// `view` is `None`.
pub struct ArenaLogic {
  controls: Arc<Mutex<Controls>>,
  view: Option<Arc<Mutex<HostView>>>,
}

impl ArenaLogic {
  pub fn new(controls: Arc<Mutex<Controls>>, view: Option<Arc<Mutex<HostView>>>) -> Self {
    Self { controls, view }
  }
}

#[async_trait]
impl StateLogic<Op, PlayerKey, Arena> for ArenaLogic {
  async fn process_input(&self, state: &mut Arena, input: LogicInput<Op, PlayerKey>) -> Result<LogicOutput<Op, PlayerKey>, StateLogicError> {
    match input {
      LogicInput::AgentJoined { agent } => {
        let Some(key) = agent.id_cloned() else {
          return Ok(LogicOutput::none());
        };
        match state.seat(key) {
          Some(seat) => {
            let policy = state.policy();
            // The board goes with the welcome. A joiner mid-round needs the
            // walls that are still standing, not the ones the round started
            // with, and `round_start` reads the live grid.
            let round = state.sim.round_start();
            Ok(LogicOutput::ops(vec![TargetedOp::new_system_to(
              key,
              vec![Op::Welcome {
                player: seat as PlayerId,
                policy,
                round: Box::new(round),
              }],
            )]))
          }
          // Said outright rather than left silent. A connection with no seat
          // receives no frames, which is indistinguishable from a broken
          // server unless somebody says so.
          None => Ok(LogicOutput::ops(vec![TargetedOp::new_system_to(key, vec![Op::NoSeat { seats: state.sim.seats() }])])),
        }
      }

      LogicInput::AgentLeft { agent_id } => {
        state.unseat(&agent_id);
        Ok(LogicOutput::none())
      }

      LogicInput::AgentOps { source, ops } => {
        let Some(key) = source.id_cloned() else {
          return Ok(LogicOutput::none());
        };
        let Some(seat) = state.seat_of(&key) else {
          return Ok(LogicOutput::none());
        };
        let mut replies = Vec::new();
        let controls = state.controls;
        for op in ops {
          // The uplink impairment. A joiner has no reason to sabotage its own
          // outbound, so loss is applied on arrival rather than at the sender.
          // `Hello` and `Ping` are control plane and are left alone: dropping a
          // version handshake makes a client look outdated for a reason that
          // has nothing to do with its version.
          let droppable = matches!(op, Op::Move { .. } | Op::DropBomb { .. });
          if droppable && controls.loss_pct > 0.0 && state.rng.unit() * 100.0 < controls.loss_pct {
            continue;
          }
          match op {
            Op::Move { seq, dir, tick } => {
              // Out-of-order *arrivals* are dropped: a straggler carries
              // nothing new, and an older direction overwriting a newer one
              // reads to the player as the controls sticking. Out-of-order
              // *execution* is a different matter and is the schedule's job.
              if state.acked.get(&key).is_some_and(|newest| seq <= *newest) {
                continue;
              }
              state.acked.insert(key, seq);
              state.sim.submit(seat, tick, Intent::Walk(dir), &controls);
            }
            Op::DropBomb { seq, tick } => {
              if state.acked.get(&key).is_some_and(|newest| seq <= *newest) {
                continue;
              }
              state.acked.insert(key, seq);
              state.sim.submit(seat, tick, Intent::Bomb, &controls);
            }
            Op::Ping { origin_ms } => {
              let server_ms = state.sim.now_ms();
              replies.push(TargetedOp::new_system_to(key, vec![Op::Pong { origin_ms, server_ms }]));
            }
            // A client announcing a wire format that is not this one cannot be
            // reasoned with, only told. Said once, on its own first message,
            // rather than as a decode warning per op for as long as it stays
            // connected.
            Op::Hello { protocol } if protocol != PROTOCOL => {
              tracing::warn!(client = protocol, server = PROTOCOL, "client is on a different wire format, telling it to reload");
              replies.push(TargetedOp::new_system_to(key, vec![Op::Outdated { server: PROTOCOL, client: protocol }]));
            }
            Op::Hello { .. } => {}
            // Everything else is server-to-client. A client sending one is
            // confused or hostile; either way it is not worth failing a tick
            // over.
            _ => {}
          }
        }
        Ok(LogicOutput::ops(replies))
      }

      LogicInput::TimeStep { delta_time } => {
        // Pick up whatever the panel changed. A seat-count change rebuilds the
        // world, so it is deliberately not live-editable here: reseating
        // everyone mid-round is a bigger hammer than a slider should be.
        let live = *self.controls.lock();
        state.controls = Controls {
          players: state.controls.players,
          ..live
        };

        let out = state.sim.advance(delta_time.as_millis() as u64, &state.controls);
        let now = state.sim.now_ms();

        // Ordered on purpose: a round reset first, then the explosions, then
        // the frame that describes the world they left behind.
        let mut outbound: Vec<Op> = Vec::new();
        if let Some(round) = out.round_start {
          outbound.push(Op::Round(Box::new(round)));
        }
        if let Some((winner, next_in_ms)) = out.round_over {
          outbound.push(Op::RoundOver { winner, next_in_ms });
        }
        for blast in out.blasts {
          outbound.push(Op::Blast(Box::new(blast)));
        }
        if let Some(frame) = out.frame {
          outbound.push(Op::Frame(Box::new(frame)));
        }

        let controls = state.controls;
        let keys: Vec<PlayerKey> = state.seats.by_seat().values().copied().collect();
        for key in keys {
          let link = state.down.entry(key).or_default();
          for op in &outbound {
            link.send(now, op.clone(), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut state.rng);
          }
        }

        let mut targeted = Vec::new();
        for (key, link) in state.down.iter_mut() {
          for op in link.drain_due(now) {
            targeted.push(TargetedOp::new_system_to(*key, vec![op]));
          }
        }
        // Acknowledgements are not impaired: reeling a prediction back in
        // should not itself be delayed, and they ride the tick because inputs
        // arrive far more often than frames go out.
        for (key, seq) in &state.acked {
          targeted.push(TargetedOp::new_system_to(*key, vec![Op::InputAck { seq: *seq }]));
        }

        if let Some(view) = &self.view {
          *view.lock() = state.host_view();
        }
        Ok(LogicOutput::ops(targeted))
      }
    }
  }
}

/// A snapshot provider that provides nothing.
///
/// State reaches a joiner as `Op::Welcome` on the tick it is seated, which
/// carries the board and the players together. Plaza asks for a snapshot on
/// join unless told otherwise, so the controller is built with that off and
/// this exists to satisfy the type.
pub struct NoSnapshots;

#[async_trait]
impl plaza::snapshot::SnapshotProvider<PlayerKey, Arena, Op> for NoSnapshots {
  async fn create_snapshot(
    &self,
    _full_state: &Arena,
    _target_agent: Option<&Agent<PlayerKey>>,
    _context: Option<plaza::snapshot::SnapshotContext>,
  ) -> Result<Option<Op>, plaza::snapshot::SnapshotError<PlayerKey>> {
    Ok(None)
  }
}

/// Unused, but `MessageTarget` has to be nameable for the logic above to
/// compile against plaza's re-exports.
#[allow(dead_code)]
fn _target_is_used(_: MessageTarget<PlayerKey>) {}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::sim::types::{Dir, B0MB_SEED, SIM_STEP_MS};
  use std::time::Duration;

  fn step(logic: &ArenaLogic, state: &mut Arena, input: LogicInput<Op, PlayerKey>) -> LogicOutput<Op, PlayerKey> {
    tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(logic.process_input(state, input)).unwrap()
  }

  fn slots(controls: Controls) -> (Arc<Mutex<Controls>>, Arc<Mutex<HostView>>) {
    (Arc::new(Mutex::new(controls)), Arc::new(Mutex::new(HostView::default())))
  }

  fn quiet() -> Controls {
    Controls {
      latency_ms: 0,
      jitter_ms: 0,
      loss_pct: 0.0,
      sync_hz: 60,
      bots: false,
      players: 2,
      ..Controls::default()
    }
  }

  fn count(out: &LogicOutput<Op, PlayerKey>, want: fn(&Op) -> bool) -> usize {
    out.ops.iter().filter(|t| t.ops.iter().any(want)).count()
  }

  /// A direction this seat can actually walk, and where it leads.
  ///
  /// Never a hardcoded `Dir::Right`: `SeatTable` does not fill from the front,
  /// so a joiner can land in any corner, and in three of the four corners the
  /// obvious direction is the border wall. A test that hardcodes one is
  /// asserting about the board layout rather than about the code under test.
  fn open_dir(state: &Arena, seat: usize) -> (Dir, crate::sim::types::Cell) {
    let here = state.sim.players[seat].cell;
    Dir::ALL
      .into_iter()
      .find_map(|d| here.step(d).filter(|c| state.sim.grid.walkable(*c)).map(|c| (d, c)))
      .expect("every spawn has a way out")
  }

  #[test]
  fn a_joiner_is_seated_welcomed_with_a_board_and_then_fed_frames() {
    let controls = quiet();
    let (cs, view) = slots(controls);
    let logic = ArenaLogic::new(cs, Some(view.clone()));
    let mut state = Arena::new(controls, B0MB_SEED);

    let joined = step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });
    let welcome = joined.ops.iter().find_map(|t| t.ops.iter().find_map(|op| match op {
      Op::Welcome { round, .. } => Some(round.clone()),
      _ => None,
    }));
    let round = welcome.expect("a joiner is welcomed");
    assert!(!round.grid.tiles.is_empty(), "and the welcome carries the board");
    assert!(!round.players.is_empty());

    let mut frames = 0;
    for _ in 0..10 {
      frames += count(&step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) }), |op| matches!(op, Op::Frame(_)));
    }
    assert!(frames > 0, "frames reach the seated player");
    assert_eq!(view.lock().seats_taken, 1, "and the host view knows who is in");
  }

  #[test]
  fn a_full_arena_says_so_rather_than_going_silent() {
    // A connection with no seat receives no frames, which is exactly what a
    // broken server looks like from the client.
    let controls = Controls { players: 1, ..quiet() };
    let (cs, _view) = slots(controls);
    let logic = ArenaLogic::new(cs, None);
    let mut state = Arena::new(controls, B0MB_SEED);

    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });
    let second = step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(2u64) });
    assert_eq!(count(&second, |op| matches!(op, Op::NoSeat { .. })), 1);
  }

  #[test]
  fn a_move_reaches_the_simulation_and_is_acknowledged() {
    let controls = Controls { input_playout: false, ..quiet() };
    let (cs, _view) = slots(controls);
    let logic = ArenaLogic::new(cs, None);
    let mut state = Arena::new(controls, B0MB_SEED);
    let agent = Agent::new_human(1u64);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: agent.clone() });

    let seat = state.seat_of(&1u64).expect("seated");
    let (dir, into) = open_dir(&state, seat);
    step(
      &logic,
      &mut state,
      LogicInput::AgentOps {
        source: agent,
        ops: vec![Op::Move { seq: 1, dir, tick: 0 }],
      },
    );
    let ticked = step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) });
    assert_eq!(count(&ticked, |op| matches!(op, Op::InputAck { seq: 1 })), 1, "the newest sequence is echoed");
    let taken = state.sim.players[seat].step.expect("and the walk started");
    assert_eq!(taken.to, into);
  }

  #[test]
  fn a_stale_sequence_is_dropped_rather_than_overwriting_a_newer_direction() {
    // Under reordering an older direction would otherwise replace a newer one,
    // which reads to the player as the controls sticking.
    let controls = Controls { input_playout: false, ..quiet() };
    let (cs, _view) = slots(controls);
    let logic = ArenaLogic::new(cs, None);
    let mut state = Arena::new(controls, B0MB_SEED);
    let agent = Agent::new_human(1u64);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: agent.clone() });

    let seat = state.seat_of(&1u64).expect("seated");
    let (newer, into) = open_dir(&state, seat);
    let older = Dir::ALL.into_iter().find(|d| *d != newer).expect("another direction");
    step(&logic, &mut state, LogicInput::AgentOps {
      source: agent.clone(),
      ops: vec![Op::Move { seq: 5, dir: newer, tick: 0 }],
    });
    step(&logic, &mut state, LogicInput::AgentOps {
      source: agent,
      ops: vec![Op::Move { seq: 2, dir: older, tick: 0 }],
    });
    step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) });
    let taken = state.sim.players[seat].step.expect("walking");
    assert_eq!(taken.to, into, "the newer direction survived the older arrival");
  }

  #[test]
  fn latency_holds_frames_back_without_dropping_them() {
    let controls = Controls { latency_ms: 200, ..quiet() };
    let (cs, _view) = slots(controls);
    let logic = ArenaLogic::new(cs, None);
    let mut state = Arena::new(controls, B0MB_SEED);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });

    let mut early = 0;
    for _ in 0..5 {
      early += count(&step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) }), |op| matches!(op, Op::Frame(_)));
    }
    assert_eq!(early, 0, "nothing delivered before the latency has elapsed");

    let mut later = 0;
    for _ in 0..20 {
      later += count(&step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) }), |op| matches!(op, Op::Frame(_)));
    }
    assert!(later > 0, "the held frames arrive once the delay passes");
  }

  #[test]
  fn a_client_on_another_wire_format_is_told_to_reload() {
    let controls = quiet();
    let (cs, _view) = slots(controls);
    let logic = ArenaLogic::new(cs, None);
    let mut state = Arena::new(controls, B0MB_SEED);
    let agent = Agent::new_human(1u64);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: agent.clone() });

    let out = step(&logic, &mut state, LogicInput::AgentOps {
      source: agent,
      ops: vec![Op::Hello { protocol: PROTOCOL.wrapping_add(1) }],
    });
    assert_eq!(count(&out, |op| matches!(op, Op::Outdated { .. })), 1);
  }

  #[test]
  fn a_departing_player_hands_the_seat_back_to_a_bot() {
    let controls = quiet();
    let (cs, view) = slots(controls);
    let logic = ArenaLogic::new(cs, Some(view.clone()));
    let mut state = Arena::new(controls, B0MB_SEED);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });
    step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) });
    assert_eq!(view.lock().seats_taken, 1);

    step(&logic, &mut state, LogicInput::AgentLeft { agent_id: 1u64 });
    step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) });
    assert_eq!(view.lock().seats_taken, 0, "the seat is free again");
  }
}

