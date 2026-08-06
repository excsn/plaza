//! The authoritative arena, as `plaza` core wants it: one `StateType` that owns
//! everything mutable, and one stateless `StateLogic` that acts on it.
//!
//! The adaptation is small, because [`sim::Server`] was already shaped for it:
//! it never reads client state, `advance` is a tick function, and inputs are
//! addressed by tick rather than applied on arrival. What this adds is seats
//! that fill and empty, and a door that can refuse.
//!
//! [`sim::Server`]: crate::sim::Server

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use plaza::session::TargetedOp;
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic, StateLogicError};
use plaza_server_utils::{SeatTable, Seating};
use plaza_session::{Delivery, DirectionProfile, LinkProfile, LinkPublisher};
use plaza_server_utils::oneshot::Pending as OneShots;

use crate::sim::protocol::{Intent, Op, ServerPolicy};
use crate::sim::server::{Server, Stats};
use crate::sim::types::{Controls, PlayerId, PlayerSnap, PlayerState, RocketState, V2};

pub type PlayerKey = u64;

/// Publishes the panel's impairment sliders to the transport that owns the
/// link. The arena states what the link should be and stops there.
pub use plaza_session::LinkSink;

/// Answers "what one-way delay did the server measure for this agent".
///
/// A closure rather than the session itself, so the arena depends on the
/// measurement and not on which transport took it. The number is the server's
/// own, never one the client reported, because it decides who gets in.
pub type RttSource = Arc<dyn Fn(&PlayerKey) -> Option<u64> + Send + Sync>;

/// Everything the omniscient half of a host needs.
///
/// A host is the server *and* a client in one process, so unlike a joiner it
/// legitimately holds both: the truth here, and its own believed state in its
/// [`NetClient`]. Drawing the two over each other is what makes the disagreement
/// visible as a thing that happened rather than a number in a panel.
///
/// [`NetClient`]: crate::net::client::NetClient
#[derive(Clone, Debug, Default)]
pub struct HostView {
  pub players: Vec<PlayerState>,
  pub rockets: Vec<RocketState>,
  pub server_now_ms: u64,
  pub stats: Stats,
  pub seats_taken: usize,
  pub seats: usize,
  /// `(accepted, late, closed, ahead, last margin)` per seat.
  pub input_verdicts: Vec<(u64, u64, u64, u64, Option<i64>)>,
  /// Where the server had everybody at the instant a client with the configured
  /// render delay is drawing.
  ///
  /// Published because an honest render error cannot be computed without it,
  /// and the buffer that answers it is the same one a shot is rewound through.
  pub truth_at_render: Vec<(PlayerId, PlayerSnap)>,
  pub refused: u64,
}

#[derive(Clone, Debug)]
pub struct Arena {
  pub sim: Server,
  pub controls: Controls,
  seats: SeatTable<PlayerKey>,
  acked: HashMap<PlayerKey, u64>,
  pending: OneShots<PlayerKey, Op>,
  pub refused: u64,
}

impl Arena {
  pub fn new(controls: Controls, seed: u64) -> Self {
    let count = controls.players.clamp(1, crate::sim::types::MAX_SEATS);
    Self {
      sim: Server::new(count, seed),
      controls,
      seats: SeatTable::new(count),
      acked: HashMap::new(),
      pending: OneShots::new(),
      refused: 0,
    }
  }

  pub fn policy(&self) -> ServerPolicy {
    crate::sim::world::policy_of(&self.controls, self.sim.seats())
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
      // Handed back to the bots rather than left frozen, so a disconnect does
      // not leave a statue standing in the arena absorbing shots.
      self.sim.release_seat(seat);
    }
    self.acked.remove(key);
    self.pending.confirm(key);
  }

  fn host_view(&self) -> HostView {
    let at = self.sim.now_ms().saturating_sub(self.controls.render_delay_ms);
    HostView {
      players: self.sim.players.clone(),
      rockets: self.sim.rockets.clone(),
      server_now_ms: self.sim.now_ms(),
      stats: self.sim.stats.clone(),
      seats_taken: self.seats.by_seat().len(),
      seats: self.sim.seats(),
      input_verdicts: self.sim.input_verdicts(),
      truth_at_render: self.sim.snaps_at(at),
      refused: self.refused,
    }
  }
}

pub struct ArenaLogic {
  controls: Arc<Mutex<Controls>>,
  view: Option<Arc<Mutex<HostView>>>,
  link: Option<LinkPublisher>,
  rtt: Option<RttSource>,
  clock: Option<Arc<AtomicU64>>,
}

impl ArenaLogic {
  pub fn new(controls: Arc<Mutex<Controls>>, view: Option<Arc<Mutex<HostView>>>) -> Self {
    Self {
      controls,
      view,
      link: None,
      rtt: None,
      clock: None,
    }
  }

  pub fn with_link(mut self, link: LinkSink) -> Self {
    self.link = Some(LinkPublisher::new(link));
    self
  }

  pub fn with_rtt(mut self, rtt: RttSource) -> Self {
    self.rtt = Some(rtt);
    self
  }

  pub fn with_clock(mut self, clock: Arc<AtomicU64>) -> Self {
    self.clock = Some(clock);
    self
  }

  fn publish_link(&self, controls: &Controls) {
    let Some(link) = &self.link else { return };
    let one_way = DirectionProfile {
      delay: Duration::from_millis(controls.latency_ms),
      jitter: Duration::from_millis(controls.jitter_ms),
      loss: controls.loss_pct / 100.0,
      delivery: if controls.datagram_link { Delivery::Datagram } else { Delivery::Reliable },
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

        // A link that cannot reach the input window is refused with both
        // numbers, because a player whose every input names a closed tick is
        // not slightly disadvantaged, they are unable to act. Letting them in
        // to discover that is worse than saying so. The measurement is the
        // server's own; a client's claim about its own latency would be the
        // one number worth lying about.
        let allowed = state.controls.playable_one_way_ms();
        let measured = self.rtt.as_ref().and_then(|f| f(&key));
        if let Some(one_way) = measured
          && one_way > allowed
        {
          state.refused += 1;
          let op = Op::Refused {
            measured_one_way_ms: one_way,
            allowed_one_way_ms: allowed,
          };
          let now = state.sim.now_ms();
          let op = state.pending.declare(key, op, now);
          return Ok(LogicOutput::ops(vec![TargetedOp::new_system_to(key, vec![op])]));
        }

        let op = match state.seat(key) {
          Some(seat) => Op::Welcome {
            player: seat as PlayerId,
            policy: state.policy(),
            start: Box::new(state.sim.start()),
          },
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
        // A client that is talking has plainly received whatever let it talk.
        // Before the seat gate, so a seatless client's traffic confirms its
        // `NoSeat` too.
        state.pending.confirm(&key);
        let Some(seat) = state.seat_of(&key) else {
          return Ok(LogicOutput::none());
        };
        let controls = state.controls;
        for op in ops {
          match op {
            Op::Move { seq, tick, dir } => {
              // Out-of-order *arrivals* are dropped: an older direction
              // overwriting a newer one reads as the controls sticking.
              // Out-of-order *execution* is the schedule's business.
              if state.acked.get(&key).is_some_and(|newest| seq <= *newest) {
                continue;
              }
              state.acked.insert(key, seq);
              state.sim.submit(seat, tick, Intent::Walk(dir), &controls);
            }
            Op::Shoot { seq, tick, aim_deg, weapon } => {
              if state.acked.get(&key).is_some_and(|newest| seq <= *newest) {
                continue;
              }
              state.acked.insert(key, seq);
              state.sim.submit(seat, tick, Intent::Shoot { aim_deg, weapon }, &controls);
            }
            // Everything else is server to client. A client sending one is
            // confused or hostile; either way it is not worth failing a tick
            // over.
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
        // The seat count rebuilds the world, so it is deliberately not live:
        // reseating everyone mid-fight is a bigger hammer than a slider.
        state.controls = Controls { players: state.controls.players, ..live };

        let out = state.sim.advance(delta_time.as_millis() as u64, &state.controls);
        let now = state.sim.now_ms();

        // Ordered on purpose: the shot, then the death it caused, then the
        // frame describing the world they left behind. A client that saw the
        // frame first would draw a corpse before it knew why.
        let mut outbound: Vec<Op> = Vec::new();
        for shot in out.shots {
          outbound.push(Op::Shot(Box::new(shot)));
        }
        for death in out.deaths {
          outbound.push(Op::Died(Box::new(death)));
        }
        for frame in out.frames {
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

/// Mean distance between what a client drew and where the server had everybody
/// **at the instant that client was drawing**.
///
/// The honest render error, live rather than in the harness. Free here: the
/// truth history it needs is the one the rewind already keeps.
pub fn honest_render_error(view: &HostView, drawn: &[(PlayerId, V2, bool)], me: PlayerId) -> Option<f32> {
  let mut sum = 0.0;
  let mut n = 0u32;
  for (id, at, _) in drawn {
    if *id == me {
      continue;
    }
    if let Some((_, snap)) = view.truth_at_render.iter().find(|(tid, _)| tid == id) {
      sum += at.dist(snap.pos);
      n += 1;
    }
  }
  (n > 0).then(|| sum / n as f32)
}

/// The same measurement taken the way this repository has always taken it:
/// against truth **now**, which charges a client for a delay it chose.
pub fn naive_render_error(view: &HostView, drawn: &[(PlayerId, V2, bool)], me: PlayerId) -> Option<f32> {
  let mut sum = 0.0;
  let mut n = 0u32;
  for (id, at, _) in drawn {
    if *id == me {
      continue;
    }
    if let Some(p) = view.players.iter().find(|p| p.id == *id) {
      sum += at.dist(p.pos);
      n += 1;
    }
  }
  (n > 0).then(|| sum / n as f32)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::sim::types::{Dir8, SIM_STEP_MS, Weapon};
  use plaza::Agent;

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
      bots: false,
      players: 2,
      ..Controls::default()
    }
  }

  fn count(out: &LogicOutput<Op, PlayerKey>, want: fn(&Op) -> bool) -> usize {
    out.ops.iter().filter(|t| t.ops.iter().any(want)).count()
  }

  const SEED: u64 = 0x9E_3C_04_71;

  #[test]
  fn a_joiner_is_seated_welcomed_and_then_fed_frames() {
    let controls = quiet();
    let view = Arc::new(Mutex::new(HostView::default()));
    let logic = ArenaLogic::new(Arc::new(Mutex::new(controls)), Some(view.clone()));
    let mut state = Arena::new(controls, SEED);

    let joined = step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });
    assert_eq!(count(&joined, |op| matches!(op, Op::Welcome { .. })), 1);

    let mut frames = 0;
    for _ in 0..20 {
      frames += count(
        &step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) }),
        |op| matches!(op, Op::Frame(_)),
      );
    }
    assert!(frames > 0);
    assert_eq!(view.lock().seats_taken, 1);
  }

  #[test]
  fn a_full_arena_says_so_rather_than_going_silent() {
    let controls = Controls { players: 1, ..quiet() };
    let logic = ArenaLogic::new(Arc::new(Mutex::new(controls)), None);
    let mut state = Arena::new(controls, SEED);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });
    let second = step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(2u64) });
    assert_eq!(count(&second, |op| matches!(op, Op::NoSeat { .. })), 1);
  }

  #[test]
  fn a_link_that_cannot_reach_the_window_is_refused_at_the_door_with_both_numbers() {
    // Refused rather than admitted and left twitching. The panel's own claim
    // is checkable from the refusal: it carries what was measured and what was
    // allowed, so nobody has to trust the verdict.
    let controls = Controls { playout_delay_ms: 100, input_max_late_ticks: 4, ..quiet() };
    let allowed = controls.playable_one_way_ms();
    let logic = ArenaLogic::new(Arc::new(Mutex::new(controls)), None).with_rtt(Arc::new(|_| Some(900)));
    let mut state = Arena::new(controls, SEED);

    let joined = step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });
    let refusal = joined
      .ops
      .iter()
      .flat_map(|t| t.ops.iter())
      .find_map(|op| match op {
        Op::Refused { measured_one_way_ms, allowed_one_way_ms } => Some((*measured_one_way_ms, *allowed_one_way_ms)),
        _ => None,
      })
      .expect("refused");
    assert_eq!(refusal, (900, allowed));
    assert_eq!(state.seat_of(&1u64), None, "and took no seat");
  }

  #[test]
  fn a_link_inside_the_window_is_admitted() {
    let controls = quiet();
    let logic = ArenaLogic::new(Arc::new(Mutex::new(controls)), None).with_rtt(Arc::new(|_| Some(20)));
    let mut state = Arena::new(controls, SEED);
    let joined = step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });
    assert_eq!(count(&joined, |op| matches!(op, Op::Welcome { .. })), 1);
  }

  #[test]
  fn an_unmeasured_link_is_admitted_rather_than_assumed_bad() {
    // `agent_rtt` returns nothing until a probe has completed, and a joiner is
    // by definition brand new. Reading that as "too slow" refuses everybody.
    let controls = quiet();
    let logic = ArenaLogic::new(Arc::new(Mutex::new(controls)), None).with_rtt(Arc::new(|_| None));
    let mut state = Arena::new(controls, SEED);
    let joined = step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });
    assert_eq!(count(&joined, |op| matches!(op, Op::Welcome { .. })), 1);
  }

  #[test]
  fn a_shot_reaches_the_simulation_and_is_acknowledged() {
    let controls = quiet();
    let logic = ArenaLogic::new(Arc::new(Mutex::new(controls)), None);
    let mut state = Arena::new(controls, SEED);
    let agent = Agent::new_human(1u64);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: agent.clone() });
    step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) });

    let tick = state.sim.now_ms() / SIM_STEP_MS + 1;
    step(
      &logic,
      &mut state,
      LogicInput::AgentOps {
        source: agent,
        ops: vec![Op::Shoot { seq: 1, tick, aim_deg: 0, weapon: Weapon::Rifle }],
      },
    );
    let ticked = step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS * 2) });
    assert_eq!(count(&ticked, |op| matches!(op, Op::InputAck { seq: 1 })), 1);
    assert_eq!(state.sim.stats.shots_fired, 1);
  }

  #[test]
  fn a_stale_sequence_is_dropped_rather_than_overwriting_a_newer_direction() {
    let controls = quiet();
    let logic = ArenaLogic::new(Arc::new(Mutex::new(controls)), None);
    let mut state = Arena::new(controls, SEED);
    let agent = Agent::new_human(1u64);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: agent.clone() });
    let seat = state.seat_of(&1u64).expect("seated");

    let tick = state.sim.now_ms() / SIM_STEP_MS + 1;
    step(&logic, &mut state, LogicInput::AgentOps {
      source: agent.clone(),
      ops: vec![Op::Move { seq: 5, tick, dir: Dir8::E }],
    });
    step(&logic, &mut state, LogicInput::AgentOps {
      source: agent,
      ops: vec![Op::Move { seq: 2, tick, dir: Dir8::W }],
    });
    step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS * 2) });
    assert_eq!(state.sim.players[seat].dir, Dir8::E, "the newer direction survived the older arrival");
  }

  #[test]
  fn a_lost_welcome_is_said_again_only_where_it_could_have_been_lost() {
    for datagram in [true, false] {
      let controls = Controls { datagram_link: datagram, ..quiet() };
      let logic = ArenaLogic::new(Arc::new(Mutex::new(controls)), None);
      let mut state = Arena::new(controls, SEED);
      step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });

      let mut repeats = 0;
      for _ in 0..80 {
        let out = step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) });
        repeats += count(&out, |op| matches!(op, Op::Welcome { .. }));
      }
      if datagram {
        assert!(repeats > 0, "a datagram link can lose it, so it is said again");
      } else {
        assert_eq!(repeats, 0, "a reliable link cannot, so saying it twice is noise");
      }
    }
  }

  #[test]
  fn traffic_from_a_client_stops_the_repeats() {
    let controls = Controls { datagram_link: true, ..quiet() };
    let logic = ArenaLogic::new(Arc::new(Mutex::new(controls)), None);
    let mut state = Arena::new(controls, SEED);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });
    step(&logic, &mut state, LogicInput::AgentOps {
      source: Agent::new_human(1u64),
      ops: vec![Op::Move { seq: 1, tick: 4, dir: Dir8::E }],
    });
    let mut repeats = 0;
    for _ in 0..80 {
      let out = step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) });
      repeats += count(&out, |op| matches!(op, Op::Welcome { .. }));
    }
    assert_eq!(repeats, 0);
  }

  #[test]
  fn the_sliders_are_published_to_the_link_rather_than_applied_here() {
    let controls = Controls { latency_ms: 200, loss_pct: 25.0, ..quiet() };
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = {
      let seen = seen.clone();
      Arc::new(move |profile: LinkProfile| seen.lock().push(profile)) as LinkSink
    };
    let logic = ArenaLogic::new(Arc::new(Mutex::new(controls)), None).with_link(sink);
    let mut state = Arena::new(controls, SEED);
    for _ in 0..3 {
      step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) });
    }
    assert_eq!(seen.lock().len(), 1, "an unchanged panel says nothing");
    let published = seen.lock()[0];
    assert_eq!(published.up.delay, Duration::from_millis(200), "one way, each direction");
    assert_eq!(published.up.loss, 0.25, "the panel reads percent, the link takes a probability");
  }

  #[test]
  fn a_departing_player_hands_the_seat_back_to_a_bot() {
    let controls = quiet();
    let view = Arc::new(Mutex::new(HostView::default()));
    let logic = ArenaLogic::new(Arc::new(Mutex::new(controls)), Some(view.clone()));
    let mut state = Arena::new(controls, SEED);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });
    step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) });
    assert_eq!(view.lock().seats_taken, 1);

    step(&logic, &mut state, LogicInput::AgentLeft { agent_id: 1u64 });
    step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) });
    assert_eq!(view.lock().seats_taken, 0, "the seat is free again");
  }

  #[test]
  fn a_cause_leaves_before_the_frame_that_already_contains_its_effect() {
    // Ordering is the whole reason a client can draw a tracer at all: a frame
    // showing a corpse, arriving before the shot that made it, is a death with
    // no visible cause.
    let controls = Controls { sync_hz: 60, ..quiet() };
    let logic = ArenaLogic::new(Arc::new(Mutex::new(controls)), None);
    let mut state = Arena::new(controls, SEED);
    let agent = Agent::new_human(1u64);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: agent.clone() });
    step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) });
    let tick = state.sim.now_ms() / SIM_STEP_MS + 1;
    step(&logic, &mut state, LogicInput::AgentOps {
      source: agent,
      ops: vec![Op::Shoot { seq: 1, tick, aim_deg: 0, weapon: Weapon::Rifle }],
    });
    let out = step(&logic, &mut state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS * 2) });

    let flat: Vec<&Op> = out.ops.iter().flat_map(|t| t.ops.iter()).collect();
    let shot = flat.iter().position(|op| matches!(op, Op::Shot(_)));
    let frame = flat.iter().position(|op| matches!(op, Op::Frame(_)));
    if let (Some(s), Some(f)) = (shot, frame) {
      assert!(s < f, "the shot must precede the frame");
    }
  }
}
