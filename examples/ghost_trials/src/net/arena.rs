//! The authoritative arena, as `plaza` core wants it.
//!
//! The thinnest of the playground arenas, and that is the finding rather than a
//! shortcut. There is no `TimeStep` work at all beyond a clock: nothing is
//! being simulated here, because a time trial has nothing to arbitrate between
//! players. The arena exists to hold the leaderboard and to **replay evidence
//! on demand**.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use plaza::session::TargetedOp;
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic, StateLogicError};
use plaza_server_utils::oneshot::Pending as OneShots;
use plaza_session::{Delivery, DirectionProfile, LinkProfile, LinkPublisher};
use plaza_server_utils::{SeatTable, Seating};

use crate::sim::log::Rejection;
use crate::sim::protocol::{Ghost, Op};
use crate::sim::server::Server;
use crate::sim::types::Controls;

pub type PlayerKey = u64;

/// Everything the omniscient half of a host needs.
#[derive(Clone, Debug, Default)]
pub struct HostView {
  pub board: Vec<Ghost>,
  pub submissions: u64,
  pub accepted: u64,
  pub refused: u64,
  pub last_refusal: Option<Rejection>,
  pub ticks_replayed: u64,
  pub bytes_in: u64,
  pub bytes_out: u64,
  pub bytes_if_paths: u64,
  pub seats_taken: usize,
  pub seats: usize,
  pub server_now_ms: u64,
  pub frames_lost: u64,
}

#[derive(Clone, Debug)]
pub struct Arena {
  pub sim: Server,
  pub controls: Controls,
  seats: SeatTable<PlayerKey>,
  /// The impairment, on the real path.
  ///
  /// It does not touch the lap, which is the example's whole claim. It decides
  /// when a ghost turns up and when a verdict lands.
  /// One-shot ops the client has not yet proved it heard.
  pending: OneShots<PlayerKey, Op>,
  /// Frames the link ate, read back from the session that ate them.
  ///
  /// Frames rather than submissions, because the link cannot see an op: it
  /// discards bytes before anything decodes them, which is exactly why this
  /// has to be read from there rather than counted here.
  pub frames_lost: u64,
}

impl Arena {
  pub fn new(controls: Controls) -> Self {
    let count = controls.players.clamp(1, 4);
    Self {
      sim: Server::new(count),
      controls,
      seats: SeatTable::new(count),
      pending: OneShots::new(),
      frames_lost: 0,
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
    self.pending.confirm(key);
  }

  fn host_view(&self) -> HostView {
    HostView {
      board: self.sim.board.clone(),
      submissions: self.sim.submissions,
      accepted: self.sim.accepted,
      refused: self.sim.refused,
      last_refusal: self.sim.last_refusal,
      ticks_replayed: self.sim.ticks_replayed,
      bytes_in: self.sim.bytes_in,
      bytes_out: self.sim.bytes_out,
      bytes_if_paths: self.sim.bytes_if_paths,
      seats_taken: self.seats.by_seat().len(),
      seats: self.sim.seats(),
      server_now_ms: self.sim.now_ms(),
      frames_lost: self.frames_lost,
    }
  }
}

/// Publishes the panel's impairment sliders to the transport that owns the
/// link. The arena states what the link should be and stops there.
pub use plaza_session::LinkSink;

/// Reads back what the link discarded. The arena cannot count this for itself:
/// a lost frame never reaches it, which is the whole point of losing it.
pub type DropCount = Arc<dyn Fn() -> u64 + Send + Sync>;

pub struct ArenaLogic {
  controls: Arc<Mutex<Controls>>,
  view: Option<Arc<Mutex<HostView>>>,
  link: Option<LinkPublisher>,
  /// Where the arena publishes its simulation clock, so the session can stamp
  /// a `Pong` with the clock clients synchronise against.
  clock: Option<Arc<AtomicU64>>,
  dropped: Option<DropCount>,
}

impl ArenaLogic {
  pub fn new(controls: Arc<Mutex<Controls>>, view: Option<Arc<Mutex<HostView>>>) -> Self {
    Self {
      controls,
      view,
      link: None,
      clock: None,
      dropped: None,
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

  /// Where to read back what the link discarded.
  pub fn with_dropped(mut self, dropped: DropCount) -> Self {
    self.dropped = Some(dropped);
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
        // Declared rather than merely sent: a datagram link can lose either,
        // and nothing else in this protocol would mention the seat again.
        let now = state.sim.now_ms();
        let op = match state.seat(key) {
          Some(seat) => state.sim.welcome(seat),
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
        let mut replies = Vec::new();
        for op in ops {
          match op {
            Op::Submit { log, claimed_ms } => {
              // A lost submission is a lap nobody recorded. There is no retry,
              // deliberately: it costs the run and never the board, because the
              // board only ever holds runs that were verified. Losing one is
              // the link's doing now, so the count comes from there: what it
              // ate never reached this arm to be counted here.
              for answer in state.sim.submit(seat, *log, claimed_ms) {
                // A verified run is everybody's: it is a ghost to race. A
                // refusal belongs to the one client that sent the log.
                let targets: Vec<PlayerKey> = match answer {
                  Op::Accepted { .. } => state.seats.by_seat().iter().map(|(_, k)| *k).collect(),
                  _ => vec![key],
                };
                for target in targets {
                  replies.push(TargetedOp::new_system_to(target, vec![answer.clone()]));
                }
              }
            }
            _ => {}
          }
        }
        Ok(LogicOutput::ops(replies))
      }

      LogicInput::TimeStep { delta_time } => {
        let live = *self.controls.lock();
        self.publish_link(&live);
        if let Some(clock) = &self.clock {
          clock.store(state.sim.now_ms(), Ordering::Relaxed);
        }
        if let Some(dropped) = &self.dropped {
          state.frames_lost = dropped();
        }
        state.controls = Controls {
          players: state.controls.players,
          ..live
        };
        // A tick moves the clock and delivers whatever the link is holding.
        // Nothing is simulated: the runs happen on the machines driving them
        // and arrive as finished evidence.
        state.sim.advance(delta_time.as_millis() as u64);
        let now = state.sim.now_ms();
        let mut out: Vec<TargetedOp<Op, PlayerKey>> = state
          .pending
          .due(now, live.datagram_link)
          .into_iter()
          .map(|(key, op)| TargetedOp::new_system_to(key, vec![op]))
          .collect();
        if let Some(view) = &self.view {
          *view.lock() = state.host_view();
        }
        Ok(LogicOutput::ops(out))
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::sim::log::{InputLog, Recorder};
  use crate::sim::rules;
  use crate::sim::types::*;
  use crate::sim::world::autopilot;
  use plaza::Agent;
  use std::time::Duration;

  fn step(logic: &ArenaLogic, state: &mut Arena, input: LogicInput<Op, PlayerKey>) -> LogicOutput<Op, PlayerKey> {
    tokio::runtime::Builder::new_current_thread()
      .build()
      .unwrap()
      .block_on(logic.process_input(state, input))
      .unwrap()
  }

  fn quiet() -> Controls {
    Controls {
      latency_ms: 0,
      jitter_ms: 0,
      loss_pct: 0.0,
      players: 2,
      ..Controls::default()
    }
  }

  fn logic() -> ArenaLogic {
    ArenaLogic::new(Arc::new(Mutex::new(quiet())), None)
  }

  fn a_run(track: &Track, version: u32) -> (InputLog, u64) {
    let mut world = rules::World::trial(track);
    let mut recorder = Recorder::new(version, Mode::Trial, track.size, 1);
    let mut finished = 0;
    for tick in 0..crate::sim::log::MAX_TICKS {
      let input = autopilot(&world.racers[0], track, world.tick, 0);
      recorder.observe(input);
      let inputs = rules::field_inputs(&world, track, input, 0);
      rules::step_world(&mut world, &inputs, track);
      if rules::finished(&world.racers[0]) {
        finished = tick;
        break;
      }
    }
    (recorder.finish(), (finished as u64 + 1) * SIM_STEP_MS)
  }

  #[test]
  fn a_joiner_is_given_the_track_and_every_ghost() {
    let logic = logic();
    let mut state = Arena::new(quiet());
    let out = step(
      &logic,
      &mut state,
      LogicInput::AgentJoined {
        agent: Agent::new_human(1u64),
      },
    );
    let ops: Vec<Op> = out.ops.into_iter().flat_map(|t| t.ops).collect();
    assert!(matches!(ops.as_slice(), [Op::Welcome { .. }]));
  }

  #[test]
  fn an_accepted_run_goes_to_every_seat_and_a_refusal_only_to_the_sender() {
    // A ghost is for racing, so everybody needs it. A refusal is a private
    // conversation with the client that sent the log.
    let logic = logic();
    let mut state = Arena::new(quiet());
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(2u64) });

    let (log, time) = a_run(&Track::circuit(), state.sim.rules_version);
    let told = submit(&logic, &mut state, 1, log.clone(), time, |op| matches!(op, Op::Accepted { .. }));
    assert_eq!(told, 2, "both seats were sent the ghost");

    let told = submit(&logic, &mut state, 1, log, 1, |op| matches!(op, Op::Refused { .. }));
    assert_eq!(told, 1, "and only the sender was told about the lie");
  }

  /// Submits a run and counts the answers it produced. Answered on the op that
  /// caused it: whatever delay the link carries has already happened on the way
  /// in, so there is nothing left to hold it for.
  fn submit(
    logic: &ArenaLogic,
    state: &mut Arena,
    key: PlayerKey,
    log: InputLog,
    claimed_ms: u64,
    want: fn(&Op) -> bool,
  ) -> usize {
    let out = step(
      logic,
      state,
      LogicInput::AgentOps {
        source: Agent::new_human(key),
        ops: vec![Op::Submit {
          log: Box::new(log),
          claimed_ms,
        }],
      },
    );
    out.ops.iter().filter(|t| t.ops.iter().any(want)).count()
  }

  /// The wiring a dead tracker passes silently: `confirm` and `due` present,
  /// `declare` missing, so nothing is ever held and the one-shot goes out once
  /// on a link that can lose it.
  #[test]
  fn a_lost_welcome_is_said_again_only_where_it_could_have_been_lost() {
    for datagram in [true, false] {
      let controls = Controls { datagram_link: datagram, ..quiet() };
      let logic = ArenaLogic::new(Arc::new(Mutex::new(controls)), None);
      let mut state = Arena::new(controls);
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
    let mut state = Arena::new(controls);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });
    let (log, time) = a_run(&Track::circuit(), state.sim.rules_version);
    step(&logic, &mut state, LogicInput::AgentOps {
      source: Agent::new_human(1u64),
      ops: vec![Op::Submit { log: Box::new(log), claimed_ms: time }],
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
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = {
      let seen = seen.clone();
      Arc::new(move |profile: LinkProfile| seen.lock().push(profile)) as LinkSink
    };
    let logic = ArenaLogic::new(Arc::new(Mutex::new(controls)), None).with_link(sink);
    let mut state = Arena::new(controls);

    for _ in 0..3 {
      step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) });
    }
    assert_eq!(seen.lock().len(), 1, "an unchanged panel says nothing");

    let published = seen.lock()[0];
    assert_eq!(published.up.delay, Duration::from_millis(200), "one way, each direction");
    assert_eq!(published.up.loss, 0.25, "the panel reads percent, the link takes a probability");
  }
  #[test]
  fn a_tick_simulates_nothing() {
    let logic = logic();
    let mut state = Arena::new(quiet());
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });
    // The welcome is confirmed, so the one thing this arena legitimately says
    // twice is out of the way and anything left is simulation.
    state.pending.confirm(&1u64);
    for _ in 0..50 {
      let out = step(
        &logic,
        &mut state,
        LogicInput::TimeStep {
          delta_time: Duration::from_millis(SIM_STEP_MS),
        },
      );
      // With nothing in flight a tick produces nothing. If this ever starts
      // failing, something has grown a simulation that does not belong here.
      assert!(out.ops.is_empty());
    }
    assert!(state.sim.now_ms() > 0, "but the clock moved");
  }
}
