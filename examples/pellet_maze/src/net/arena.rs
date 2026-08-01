//! The authoritative arena, as `plaza` core wants it.
//!
//! The same shape as `bomb_grid`'s, which is the point: the netcode wrapper is
//! boilerplate once the simulation is shaped for it, and the only genuinely new
//! thing this arena sends is [`Op::TurnTaken`], the place a turn happened.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use plaza::session::{MessageTarget, TargetedOp};
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic, StateLogicError};
use plaza::Agent;
use playground_common::oneshot::Pending as OneShots;
use plaza_session::{Delivery, DirectionProfile, LinkProfile};
use plaza_server_utils::{SeatTable, Seating};

use crate::sim::protocol::{Op, ServerPolicy};
use crate::sim::server::{Audience, Server};
use crate::sim::types::{Cell, Controls, Maze, PlayerId, PlayerState, MATCH_ROUNDS};

pub type PlayerKey = u64;

/// Everything the omniscient half of a host needs.
#[derive(Clone, Debug, Default)]
pub struct HostView {
  pub maze: Maze,
  pub players: Vec<PlayerState>,
  pub pellets: Vec<Cell>,
  pub round: u32,
  pub server_now_ms: u64,

  pub turns_taken: u64,
  pub turns_expired: u64,
  pub catches: u64,
  pub pellets_eaten: u64,
  /// Pursuers eaten by an energized runner. The counter that says whether the
  /// role inversion is ever actually reached, as opposed to merely implemented.
  pub devoured: u64,
  pub match_round: u32,
  pub match_rounds: u32,
  /// Per seat: `(taken, expired)`. The buffer's two failure modes, which say
  /// opposite things and must not be averaged together.
  pub turn_stats: Vec<(u64, u64)>,
  pub input_verdicts: Vec<(u64, u64, u64, u64, Option<i64>)>,
  pub seats_taken: usize,
  pub seats: usize,
}

#[derive(Clone, Debug)]
pub struct Arena {
  pub sim: Server,
  pub controls: Controls,
  seats: SeatTable<PlayerKey>,
  acked: HashMap<PlayerKey, u64>,
  /// One-shot ops the client has not yet proved it heard. Only a datagram
  /// link can lose one; see [`OneShots`].
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
      // Told, never assumed. The buffer decides whether a turn is taken or
      // forgotten, so a client guessing differently would predict a turn the
      // server dropped and then run down a corridor the server never entered.
      turn_buffer_ms: self.controls.turn_buffer_ms,
      input_max_late_ticks: self.controls.input_max_late_ticks,
      input_max_early_ticks: self.controls.input_max_early_ticks,
      players: self.sim.seats(),
    }
  }

  pub fn seat_of(&self, key: &PlayerKey) -> Option<usize> {
    self.seats.seat_of(key)
  }

  fn seat(&mut self, key: PlayerKey) -> Option<usize> {
    let seating = self.seats.seat(key);
    if let Seating::Fresh(seat) = seating {
      self.sim.take_seat(seat);
    }
    seating.index()
  }

  fn unseat(&mut self, key: &PlayerKey) {
    if let Some(seat) = self.seats.unseat(key) {
      self.sim.release_seat(seat);
    }
    self.acked.remove(key);
    self.pending.confirm(key);
  }

  fn host_view(&self) -> HostView {
    HostView {
      maze: self.sim.maze.clone(),
      players: self.sim.players.clone(),
      pellets: self.sim.pellets.clone(),
      round: self.sim.round(),
      server_now_ms: self.sim.now_ms(),
      turns_taken: self.sim.turns_taken,
      turns_expired: self.sim.turns_expired,
      catches: self.sim.catches,
      pellets_eaten: self.sim.pellets_eaten,
      devoured: self.sim.devoured,
      match_round: self.sim.match_round(),
      match_rounds: MATCH_ROUNDS,
      turn_stats: self.sim.turn_stats(),
      input_verdicts: self.sim.input_verdicts(),
      seats_taken: self.seats.by_seat().len(),
      seats: self.sim.seats(),
    }
  }
}

/// Publishes the panel's impairment sliders to the transport that owns the
/// link. The arena states what the link should be and stops there.
pub type LinkSink = Arc<dyn Fn(LinkProfile) + Send + Sync>;

pub struct ArenaLogic {
  controls: Arc<Mutex<Controls>>,
  view: Option<Arc<Mutex<HostView>>>,
  link: Option<LinkSink>,
  /// The profile last published, so an unchanged panel says nothing.
  published: Mutex<Option<LinkProfile>>,
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
      published: Mutex::new(None),
      clock: None,
    }
  }

  /// Where the impairment sliders take effect.
  pub fn with_link(mut self, link: LinkSink) -> Self {
    self.link = Some(link);
    self
  }

  /// Where to publish the simulation clock for the session to read.
  pub fn with_clock(mut self, clock: Arc<AtomicU64>) -> Self {
    self.clock = Some(clock);
    self
  }

  /// Pushes the panel's link settings down to the transport when they change.
  fn publish_link(&self, controls: &Controls) {
    let Some(sink) = &self.link else { return };
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
    let profile = LinkProfile::symmetric(one_way);
    let mut published = self.published.lock();
    if *published == Some(profile) {
      return;
    }
    *published = Some(profile);
    sink(profile);
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
        let now = state.sim.now_ms();
        let op = match state.seat(key) {
          Some(seat) => Op::Welcome {
            player: seat as PlayerId,
            policy: state.policy(),
            round: Box::new(state.sim.round_start()),
          },
          None => Op::NoSeat { seats: state.sim.seats() },
        };
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
        let controls = state.controls;
        for op in ops {
          match op {
            Op::Turn { seq, dir, tick } => {
              if state.acked.get(&key).is_some_and(|newest| seq <= *newest) {
                continue;
              }
              state.acked.insert(key, seq);
              state.sim.submit(seat, tick, dir, &controls);
            }
            _ => {}
          }
        }
        Ok(LogicOutput::none())
      }

      LogicInput::TimeStep { delta_time } => {
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

        let mut outbound: Vec<Op> = Vec::new();
        if let Some(round) = out.round_start {
          outbound.push(Op::Round(Box::new(round)));
        }
        if let Some((runner, by, next_in_ms)) = out.caught {
          outbound.push(Op::Caught { runner, by, next_in_ms });
        }
        // Two lists, because secrecy is a property of who is *sent* a
        // message. A turn report is only ever read by the player it names, and
        // an event about an invisible player must reach nobody else.
        let mut private: Vec<(PlayerId, Op)> = Vec::new();
        for taken in out.turns {
          private.push((taken.player, Op::TurnTaken(Box::new(taken))));
        }
        for eaten in out.eaten {
          let op = Op::Eaten { by: eaten.by, cells: eaten.cells };
          match eaten.audience {
            Audience::Everyone => outbound.push(op),
            Audience::Only(id) => private.push((id, op)),
          }
        }
        for power in out.powers {
          let op = Op::PowerTaken {
            by: power.by,
            cell: power.cell,
            kind: power.kind,
            until_ms: power.until_ms,
          };
          match power.audience {
            Audience::Everyone => outbound.push(op),
            Audience::Only(id) => private.push((id, op)),
          }
        }
        for (runner, pursuer) in out.devoured {
          outbound.push(Op::Devoured { runner, pursuer });
        }
        if let Some((standings, next_in_ms)) = out.match_over {
          outbound.push(Op::MatchOver { standings, next_in_ms });
        }

        // Seat by seat, because the **frame is per recipient**: a hidden runner
        // is absent from everybody else's copy, which is the only way to keep
        // the secret. Everything else is the same message for all of them.
        let mut targeted = Vec::new();
        let seated: Vec<(usize, PlayerKey)> = state.seats.by_seat().iter().map(|(seat, key)| (*seat, *key)).collect();
        for (seat, key) in seated {
          let mine = out.frames.iter().find(|(id, _)| *id as usize == seat).map(|(_, f)| f.clone());
          for op in &outbound {
            targeted.push(TargetedOp::new_system_to(key, vec![op.clone()]));
          }
          // Before the frame: a turn describes a tick the frame is about to
          // summarise, and a client that learned the position first would
          // compare against a turn it had not been told about.
          for (id, op) in private.iter().filter(|(id, _)| *id as usize == seat) {
            let _ = id;
            targeted.push(TargetedOp::new_system_to(key, vec![op.clone()]));
          }
          if let Some(frame) = mine {
            targeted.push(TargetedOp::new_system_to(key, vec![Op::Frame(Box::new(frame))]));
          }
        }
        targeted.extend(
          state
            .pending
            .due(now, live.datagram_link)
            .into_iter()
            .map(|(key, op)| TargetedOp::new_system_to(key, vec![op])),
        );
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

/// State reaches a joiner as `Op::Welcome`, which carries the maze, the players
/// and the pellets together.
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

#[allow(dead_code)]
fn _target_is_used(_: MessageTarget<PlayerKey>) {}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::sim::protocol::PROTOCOL;
  use crate::sim::types::{MAZE_SEED, SIM_STEP_MS};
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

  /// The wiring a dead tracker passes silently: `confirm` and `due` present,
  /// `declare` missing, so nothing is ever held and the one-shot goes out once
  /// on a link that can lose it.
  #[test]
  fn a_lost_welcome_is_said_again_only_where_it_could_have_been_lost() {
    for datagram in [true, false] {
      let controls = Controls { datagram_link: datagram, ..quiet() };
      let logic = ArenaLogic::new(Arc::new(Mutex::new(controls)), None);
      let mut state = Arena::new(controls, MAZE_SEED);
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
    let mut state = Arena::new(controls, MAZE_SEED);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });
    step(&logic, &mut state, LogicInput::AgentOps {
      source: Agent::new_human(1u64),
      ops: vec![Op::Turn { seq: 1, dir: crate::sim::types::Dir::Right, tick: 0 }],
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
    let controls = Controls { latency_ms: 200, jitter_ms: 40, loss_pct: 25.0, ..quiet() };
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = {
      let seen = seen.clone();
      Arc::new(move |profile: LinkProfile| seen.lock().push(profile)) as LinkSink
    };
    let shared = Arc::new(Mutex::new(controls));
    let logic = ArenaLogic::new(shared.clone(), None).with_link(sink);
    let mut state = Arena::new(controls, MAZE_SEED);

    for _ in 0..3 {
      step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) });
    }
    assert_eq!(seen.lock().len(), 1, "an unchanged panel says nothing");

    let published = seen.lock()[0];
    assert_eq!(published.up, published.down, "the sliders describe a round trip");
    assert_eq!(published.up.delay, Duration::from_millis(200), "one way, each direction");
    assert_eq!(published.up.loss, 0.25, "the panel reads percent, the link takes a probability");

    shared.lock().latency_ms = 40;
    step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) });
    assert_eq!(seen.lock().len(), 2, "a dragged slider is published once more");
    assert_eq!(seen.lock()[1].up.delay, Duration::from_millis(40));
  }

  fn count(out: &LogicOutput<Op, PlayerKey>, want: fn(&Op) -> bool) -> usize {
    out.ops.iter().filter(|t| t.ops.iter().any(want)).count()
  }

  #[test]
  fn a_joiner_is_seated_welcomed_with_a_maze_and_then_fed_frames() {
    let controls = quiet();
    let (cs, view) = slots(controls);
    let logic = ArenaLogic::new(cs, Some(view.clone()));
    let mut state = Arena::new(controls, MAZE_SEED);

    let joined = step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });
    let round = joined
      .ops
      .iter()
      .find_map(|t| {
        t.ops.iter().find_map(|op| match op {
          Op::Welcome { round, .. } => Some(round.clone()),
          _ => None,
        })
      })
      .expect("a joiner is welcomed");
    assert!(!round.maze.tiles.is_empty(), "the welcome carries the maze");
    assert!(!round.pellets.is_empty(), "and the pellets");

    let mut frames = 0;
    for _ in 0..10 {
      frames += count(
        &step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) }),
        |op| matches!(op, Op::Frame(_)),
      );
    }
    assert!(frames > 0);
    assert_eq!(view.lock().seats_taken, 1);
  }

  #[test]
  fn a_turn_reaches_the_simulation_and_its_place_is_broadcast() {
    let controls = Controls { input_playout: false, ..quiet() };
    let (cs, _view) = slots(controls);
    let logic = ArenaLogic::new(cs, None);
    let mut state = Arena::new(controls, MAZE_SEED);
    let agent = Agent::new_human(1u64);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: agent.clone() });

    let seat = state.seat_of(&1u64).expect("seated");
    let at = state.sim.players[seat].cell;
    let heading = state.sim.players[seat].heading;
    let want = state.sim.maze.exits(at).into_iter().find(|d| *d != heading).expect("a turn");

    step(&logic, &mut state, LogicInput::AgentOps {
      source: agent,
      ops: vec![Op::Turn { seq: 1, dir: want, tick: 0 }],
    });

    let mut reported = 0;
    for _ in 0..200 {
      reported += count(
        &step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) }),
        |op| matches!(op, Op::TurnTaken(_)),
      );
      if reported > 0 {
        break;
      }
    }
    assert!(reported > 0, "the place was broadcast");
  }

  #[test]
  fn a_full_arena_says_so_rather_than_going_silent() {
    let controls = Controls { players: 1, ..quiet() };
    let (cs, _view) = slots(controls);
    let logic = ArenaLogic::new(cs, None);
    let mut state = Arena::new(controls, MAZE_SEED);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });
    let second = step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(2u64) });
    assert_eq!(count(&second, |op| matches!(op, Op::NoSeat { .. })), 1);
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
  fn a_departing_player_hands_the_seat_back() {
    let controls = quiet();
    let (cs, view) = slots(controls);
    let logic = ArenaLogic::new(cs, Some(view.clone()));
    let mut state = Arena::new(controls, MAZE_SEED);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });
    step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) });
    assert_eq!(view.lock().seats_taken, 1);

    step(&logic, &mut state, LogicInput::AgentLeft { agent_id: 1u64 });
    step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) });
    assert_eq!(view.lock().seats_taken, 0);
  }

  #[test]
  fn the_policy_tells_a_client_the_turn_buffer() {
    // A client that guessed this would predict turns the server had already
    // forgotten, which is a wrong junction manufactured out of a mismatched
    // constant rather than out of the network.
    let controls = Controls { turn_buffer_ms: 321, ..quiet() };
    let (cs, _view) = slots(controls);
    let logic = ArenaLogic::new(cs, None);
    let state = Arena::new(controls, MAZE_SEED);
    let _ = logic;
    assert_eq!(state.policy().turn_buffer_ms, 321);
  }
}
