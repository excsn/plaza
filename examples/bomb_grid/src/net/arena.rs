//! The authoritative arena, as `plaza` core wants it: one `StateType` that owns
//! everything mutable, and one stateless `StateLogic` that acts on it.
//!
//! The adaptation is small, because [`sim::Server`] was already shaped for it:
//! it never reads client state, `advance` is a tick function, and inputs are
//! already addressed by tick rather than applied on arrival. What this adds is
//! seats that fill and empty.
//!
//! [`sim::Server`]: crate::sim::Server

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use plaza::session::{MessageTarget, TargetedOp};
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic, StateLogicError};
use plaza::Agent;
use plaza_server_utils::oneshot::Pending as OneShots;
use plaza_session::{Delivery, DirectionProfile, LinkProfile, LinkPublisher};
use plaza_server_utils::{SeatTable, Seating};

use crate::sim::protocol::{Intent, Op, ServerPolicy};
use crate::sim::server::Server;
use crate::sim::types::{BombState, Cell, Controls, Grid, PlayerId, PlayerState, PowerupState};

/// How a connection is identified. Assigned by the server on accept, never
/// supplied by the client.
pub type PlayerKey = u64;

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
  /// One-shot ops the client has not yet proved it heard.
  pending: OneShots<PlayerKey, Op>,
}

impl Arena {
  pub fn new(controls: Controls, seed: u64) -> Self {
    let count = controls.players.clamp(1, 4);
    Self {
      sim: Server::new(count, seed),
      controls,
      seats: SeatTable::new(count),
      acked: HashMap::new(),
      pending: OneShots::new(),
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
    self.pending.confirm(key);
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
/// Publishes the panel's impairment sliders to the transport that owns the
/// link. The arena states what the link should be and stops there.
pub use plaza_session::LinkSink;

pub struct ArenaLogic {
  controls: Arc<Mutex<Controls>>,
  view: Option<Arc<Mutex<HostView>>>,
  link: Option<LinkPublisher>,
  /// Where the arena publishes its simulation clock, so the session can stamp
  /// a `Pong` with the clock clients synchronise against.
  clock: Option<Arc<AtomicU64>>,
}

impl ArenaLogic {
  pub fn new(controls: Arc<Mutex<Controls>>, view: Option<Arc<Mutex<HostView>>>) -> Self {
    Self {
      controls,
      view,
      link: None,
      clock: None,
    }
  }

  /// Where the impairment sliders take effect.
  pub fn with_link(mut self, link: LinkSink) -> Self {
    self.link = Some(LinkPublisher::new(link));
    self
  }

  /// Where to publish the simulation clock for the session to read.
  pub fn with_clock(mut self, clock: Arc<AtomicU64>) -> Self {
    self.clock = Some(clock);
    self
  }

  /// Pushes the panel's link settings down to the transport when they change.
  fn publish_link(&self, controls: &Controls) {
    let Some(link) = &self.link else { return };
    // One way, applied in each direction, which is what the slider has always
    // meant here.
    let one_way = DirectionProfile {
      delay: Duration::from_millis(controls.latency_ms),
      jitter: Duration::from_millis(controls.jitter_ms),
      loss: controls.loss_pct / 100.0,
      delivery: if controls.datagram_link {
        Delivery::Datagram
      } else {
        Delivery::Reliable
      },
    };
    link.publish(LinkProfile::symmetric(one_way));
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
        let op = match state.seat(key) {
          Some(seat) => {
            let policy = state.policy();
            // The board goes with the welcome. A joiner mid-round needs the
            // walls that are still standing, not the ones the round started
            // with, and `round_start` reads the live grid.
            let round = state.sim.round_start();
            Op::Welcome {
              player: seat as PlayerId,
              policy,
              round: Box::new(round),
            }
          }
          // Said outright rather than left silent. A connection with no seat
          // receives no frames, which is indistinguishable from a broken
          // server unless somebody says so.
          None => Op::NoSeat { seats: state.sim.seats() },
        };
        // Declared rather than merely sent: a datagram link can lose it, and
        // nothing else in this protocol would ever mention the seat again.
        let now = state.sim.now_ms();
        let op = state.pending.declare(key, op, now);
        Ok(LogicOutput::ops(vec![TargetedOp::new_system_to(key, vec![op])]))
      }

      LogicInput::AgentLeft { agent_id } => {
        state.unseat(&agent_id);
        Ok(LogicOutput::none())
      }

      LogicInput::AgentOps { source, ops } => {
        let Some(key) = source.id_cloned() else {
          return Ok(LogicOutput::none());
        };
        // A client that is talking has plainly received whatever let it talk, so
        // this is the acknowledgement and no ack op has to exist. Before the
        // seat gate: a seatless client's traffic confirms its `NoSeat` too, and
        // that verdict is just as unrepeatable as a welcome.
        state.pending.confirm(&key);
        let Some(seat) = state.seat_of(&key) else {
          return Ok(LogicOutput::none());
        };
        let mut replies = Vec::new();
        let controls = state.controls;
        for op in ops {
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
        self.publish_link(&live);
        if let Some(clock) = &self.clock {
          clock.store(state.sim.now_ms(), Ordering::Relaxed);
        }
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

        let mut targeted = Vec::new();
        let keys: Vec<PlayerKey> = state.seats.by_seat().values().copied().collect();
        for key in keys {
          for op in &outbound {
            targeted.push(TargetedOp::new_system_to(key, vec![op.clone()]));
          }
        }
        targeted.extend(
          state
            .pending
            .due(now, live.datagram_link)
            .into_iter()
            .map(|(key, op)| TargetedOp::new_system_to(key, vec![op])),
        );
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

/// Unused, but `MessageTarget` has to be nameable for the logic above to
/// compile against plaza's re-exports.
#[allow(dead_code)]
fn _target_is_used(_: MessageTarget<PlayerKey>) {}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::sim::protocol::PROTOCOL;
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

  /// The wiring a dead tracker passes silently: `confirm` and `due` present,
  /// `declare` missing, so nothing is ever held and the one-shot goes out once
  /// on a link that can lose it.
  #[test]
  fn a_lost_welcome_is_said_again_only_where_it_could_have_been_lost() {
    for datagram in [true, false] {
      let controls = Controls { datagram_link: datagram, ..quiet() };
      let logic = ArenaLogic::new(Arc::new(Mutex::new(controls)), None);
      let mut state = Arena::new(controls, B0MB_SEED);
      step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });

      let mut repeats = 0;
      for _ in 0..80 {
        let out = step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) });
        repeats += out.ops.iter().filter(|t| t.ops.iter().any(|op| matches!(op, Op::Welcome { .. }))).count();
      }
      if datagram {
        assert!(repeats > 0, "a datagram link can lose it, so it is said again");
      } else {
        assert_eq!(repeats, 0, "a reliable link cannot, so saying it twice is noise");
      }
    }
  }

  /// The other half of the contract, and the half whose absence is silent: a
  /// welcome that is never confirmed is repeated into a client that treats it
  /// as a fresh start, so the first seconds of play rebuild the world over and
  /// over. The guard above only asserts that repeats happen.
  #[test]
  fn traffic_from_a_client_stops_the_repeats() {
    let controls = Controls { datagram_link: true, ..quiet() };
    let logic = ArenaLogic::new(Arc::new(Mutex::new(controls)), None);
    let mut state = Arena::new(controls, B0MB_SEED);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });
    step(&logic, &mut state, LogicInput::AgentOps {
      source: Agent::new_human(1u64),
      ops: vec![Op::Move { seq: 1, dir: Dir::Right, tick: 0 }],
    });

    let mut repeats = 0;
    for _ in 0..80 {
      let out = step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) });
      repeats += out.ops.iter().filter(|t| t.ops.iter().any(|op| matches!(op, Op::Welcome { .. }))).count();
    }
    assert_eq!(repeats, 0, "confirmed, so nothing is repeated");
  }

  /// What the arena still owns of impairment: turning the panel's numbers into
  /// a link profile, once, and only when they change. Holding the frames back
  /// is the session's, and is tested where that happens.
  #[test]
  fn the_sliders_are_published_to_the_link_rather_than_applied_here() {
    let controls = Controls { latency_ms: 200, loss_pct: 25.0, ..quiet() };
    let (cs, _view) = slots(controls);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = {
      let seen = seen.clone();
      Arc::new(move |profile: LinkProfile| seen.lock().push(profile)) as LinkSink
    };
    let logic = ArenaLogic::new(cs, None).with_link(sink);
    let mut state = Arena::new(controls, B0MB_SEED);

    for _ in 0..3 {
      step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) });
    }
    assert_eq!(seen.lock().len(), 1, "an unchanged panel says nothing");

    let published = seen.lock()[0];
    assert_eq!(published.up.delay, Duration::from_millis(200), "one way, each direction");
    assert_eq!(published.up.loss, 0.25, "the panel reads percent, the link takes a probability");
  }

  #[test]
  fn the_handshake_survives_a_disagreement_about_ops() {
    use plaza_wire::frame::{self, ProtocolVersion};
    use plaza_wire::{MsgPackCodec, WireCodec};

    let mut out = Vec::new();
    frame::begin(frame::Kind::Hello, &mut out);
    MsgPackCodec.encode_into(&ProtocolVersion(PROTOCOL), &mut out).expect("client encode");

    let (tag, body) = frame::split(&out).expect("non-empty");
    assert_eq!(frame::Kind::from_byte(tag), Some(frame::Kind::Hello));
    let theirs: ProtocolVersion = MsgPackCodec.decode(body).expect("server decode");
    assert_eq!(theirs, ProtocolVersion(PROTOCOL));

    assert!(!ProtocolVersion(PROTOCOL).agrees_with(ProtocolVersion(PROTOCOL.wrapping_add(1))));
    assert!(ProtocolVersion(PROTOCOL).agrees_with(ProtocolVersion::UNKNOWN));
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

