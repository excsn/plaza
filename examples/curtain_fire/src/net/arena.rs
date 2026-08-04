//! The authoritative field, as `plaza` core wants it.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use plaza::Agent;
use plaza::session::TargetedOp;
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic, StateLogicError};
use plaza_server_utils::{SeatTable, Seating};
use plaza_session::{Delivery, DirectionProfile, LinkProfile};
use playground_common::oneshot::Pending as OneShots;

use crate::sim::curtain::{Bullet, Downed, Wave};
use crate::sim::protocol::{Intent, Op, ServerPolicy, wire_cost};
use crate::sim::server::{Server, Stats};
use crate::sim::types::{Controls, PlayerBullet, PlayerId, Ship};

pub type PlayerKey = u64;
pub type LinkSink = Arc<dyn Fn(LinkProfile) + Send + Sync>;
pub type RttSource = Arc<dyn Fn(&PlayerKey) -> Option<u64> + Send + Sync>;

#[derive(Clone, Debug, Default)]
pub struct HostView {
  pub ships: Vec<Ship>,
  pub bullets: Vec<PlayerBullet>,
  /// The server's own derived curtain, for drawing the host's omniscient half
  /// over the client's belief.
  pub curtain: Vec<Bullet>,
  pub waves: Vec<Wave>,
  pub downed: Vec<Downed>,
  pub server_now_ms: u64,
  pub tick: u64,
  pub stats: Stats,
  pub seats_taken: usize,
  pub seats: usize,
  pub input_verdicts: Vec<(u64, u64, u64, u64, Option<i64>)>,
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
    self.sim.policy(&self.controls)
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
      ships: self.sim.ships.clone(),
      bullets: self.sim.bullets.clone(),
      curtain: self.sim.curtain().to_vec(),
      waves: self.sim.waves.clone(),
      downed: self.sim.downed.clone(),
      server_now_ms: self.sim.now_ms(),
      tick: self.sim.tick(),
      stats: self.sim.stats.clone(),
      seats_taken: self.seats.by_seat().len(),
      seats: self.sim.seats(),
      input_verdicts: self.sim.input_verdicts(),
      refused: self.refused,
    }
  }
}

pub struct ArenaLogic {
  controls: Arc<Mutex<Controls>>,
  view: Option<Arc<Mutex<HostView>>>,
  link: Option<LinkSink>,
  rtt: Option<RttSource>,
  published: Mutex<Option<LinkProfile>>,
  clock: Option<Arc<AtomicU64>>,
}

impl ArenaLogic {
  pub fn new(controls: Arc<Mutex<Controls>>, view: Option<Arc<Mutex<HostView>>>) -> Self {
    Self {
      controls,
      view,
      link: None,
      rtt: None,
      published: Mutex::new(None),
      clock: None,
    }
  }

  pub fn with_link(mut self, link: LinkSink) -> Self {
    self.link = Some(link);
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
    let Some(sink) = &self.link else { return };
    let one_way = DirectionProfile {
      delay: Duration::from_millis(controls.latency_ms),
      jitter: Duration::from_millis(controls.jitter_ms),
      loss: controls.loss_pct / 100.0,
      delivery: if controls.datagram_link { Delivery::Datagram } else { Delivery::Reliable },
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
        let allowed = state.controls.playable_one_way_ms();
        if let Some(one_way) = self.rtt.as_ref().and_then(|f| f(&key))
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
          // The welcome carries every wave already in flight. A joiner given
          // only future waves derives an empty field and flies through a
          // curtain it cannot see, and nothing in the frames it receives would
          // ever say so.
          Some(seat) => Op::Welcome {
            player: seat as PlayerId,
            policy: state.policy(),
            start: Box::new(state.sim.start()),
          },
          None => Op::NoSeat { seats: state.sim.seats() },
        };
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
        state.pending.confirm(&key);
        let Some(seat) = state.seat_of(&key) else {
          return Ok(LogicOutput::none());
        };
        let controls = state.controls;
        for op in ops {
          let (seq, tick, intent) = match op {
            Op::Move { seq, tick, dir } => (seq, tick, Intent::Move(dir)),
            Op::Fire { seq, tick } => (seq, tick, Intent::Fire),
            Op::Struck { seq, tick } => (seq, tick, Intent::Struck),
            _ => continue,
          };
          if state.acked.get(&key).is_some_and(|newest| seq <= *newest) {
            continue;
          }
          state.acked.insert(key, seq);
          state.sim.submit(seat, tick, intent, &controls);
        }
        Ok(LogicOutput::none())
      }

      LogicInput::TimeStep { delta_time } => {
        let live = *self.controls.lock();
        self.publish_link(&live);
        if let Some(clock) = &self.clock {
          clock.store(state.sim.now_ms(), Ordering::Relaxed);
        }
        state.controls = Controls { players: state.controls.players, ..live };

        let out = state.sim.advance(delta_time.as_millis() as u64, &state.controls);
        let now = state.sim.now_ms();

        // Causes before consequences. A wave has to arrive before the frame
        // whose ships are already dodging it, or the curtain appears out of
        // nothing and the client's derivation starts mid-air.
        let mut outbound: Vec<Op> = Vec::new();
        for wave in out.waves {
          outbound.push(Op::WaveUp(Box::new(wave)));
        }
        for down in out.downed {
          outbound.push(Op::ArmDown(down));
        }
        for death in out.deaths {
          outbound.push(Op::Died(Box::new(death)));
        }
        for frame in out.frames {
          outbound.push(Op::Frame(Box::new(frame)));
        }

        // Priced here, where every outbound op passes through one place. The
        // split is the example's headline and it cannot be taken anywhere else.
        if !outbound.is_empty() {
          let derivable: Vec<Op> = outbound.iter().filter(|op| wire_cost::is_derivable_half(op)).cloned().collect();
          let streamed: Vec<Op> = outbound.iter().filter(|op| !wire_cost::is_derivable_half(op)).cloned().collect();
          let stats = &mut state.sim.stats;
          stats.bytes_derivable += wire_cost::bytes(&derivable) as u64;
          stats.bytes_streamed += wire_cost::bytes(&streamed) as u64;
          stats.bytes_total += wire_cost::bytes(&outbound) as u64;
          stats.bytes_numerically_tagged += wire_cost::bytes_numerically_tagged(&outbound) as u64;
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

#[cfg(test)]
mod tests {
  use super::*;
  use crate::sim::types::{DeathRule, Dir8, SIM_STEP_MS};

  const SEED: u64 = 0x11_22_33_44;

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

  fn tick(logic: &ArenaLogic, state: &mut Arena) -> LogicOutput<Op, PlayerKey> {
    step(logic, state, LogicInput::TimeStep { delta_time: Duration::from_millis(SIM_STEP_MS) })
  }

  #[test]
  fn a_joiner_is_welcomed_with_every_wave_already_in_flight() {
    let controls = quiet();
    let logic = ArenaLogic::new(Arc::new(Mutex::new(controls)), None);
    let mut state = Arena::new(controls, SEED);
    for _ in 0..200 {
      tick(&logic, &mut state);
    }
    assert!(!state.sim.waves.is_empty(), "a wave is up");

    let joined = step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });
    let start = joined
      .ops
      .iter()
      .flat_map(|t| t.ops.iter())
      .find_map(|op| match op {
        Op::Welcome { start, .. } => Some(start.clone()),
        _ => None,
      })
      .expect("welcomed");
    assert_eq!(start.waves.len(), state.sim.waves.len(), "the joiner would have derived an empty field");
  }

  #[test]
  fn a_wave_leaves_before_the_frame_whose_ships_are_already_dodging_it() {
    let controls = quiet();
    let logic = ArenaLogic::new(Arc::new(Mutex::new(controls)), None);
    let mut state = Arena::new(controls, SEED);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });

    for _ in 0..200 {
      let out = tick(&logic, &mut state);
      let flat: Vec<&Op> = out.ops.iter().flat_map(|t| t.ops.iter()).collect();
      let wave = flat.iter().position(|op| matches!(op, Op::WaveUp(_)));
      let frame = flat.iter().position(|op| matches!(op, Op::Frame(_)));
      if let (Some(w), Some(f)) = (wave, frame) {
        assert!(w < f, "the frame arrived before the wave that explains it");
        return;
      }
    }
  }

  #[test]
  fn the_panel_prices_the_two_halves_separately() {
    let controls = quiet();
    let logic = ArenaLogic::new(Arc::new(Mutex::new(controls)), None);
    let mut state = Arena::new(controls, SEED);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });
    for _ in 0..400 {
      tick(&logic, &mut state);
    }
    assert!(state.sim.stats.bytes_derivable > 0, "no wave was ever priced");
    assert!(state.sim.stats.bytes_streamed > state.sim.stats.bytes_derivable, "the streamed half should dominate the total");
    assert!(state.sim.stats.bytes_numerically_tagged < state.sim.stats.bytes_total);
  }

  #[test]
  fn a_declaration_reaches_the_simulation() {
    let controls = Controls { death_rule: DeathRule::ClientDeclares, ..quiet() };
    let logic = ArenaLogic::new(Arc::new(Mutex::new(controls)), None);
    let mut state = Arena::new(controls, SEED);
    let agent = Agent::new_human(1u64);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: agent.clone() });
    for _ in 0..10 {
      tick(&logic, &mut state);
    }
    let at = state.sim.tick();
    step(&logic, &mut state, LogicInput::AgentOps {
      source: agent,
      ops: vec![Op::Struck { seq: 1, tick: at }],
    });
    assert_eq!(state.sim.stats.declared, 1);
  }

  #[test]
  fn a_link_that_cannot_reach_the_window_is_refused_with_both_numbers() {
    let controls = quiet();
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
  }

  #[test]
  fn an_unmeasured_link_is_admitted_rather_than_assumed_bad() {
    let controls = quiet();
    let logic = ArenaLogic::new(Arc::new(Mutex::new(controls)), None).with_rtt(Arc::new(|_| None));
    let mut state = Arena::new(controls, SEED);
    let joined = step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });
    assert_eq!(count(&joined, |op| matches!(op, Op::Welcome { .. })), 1);
  }

  #[test]
  fn a_stale_sequence_is_dropped() {
    let controls = quiet();
    let logic = ArenaLogic::new(Arc::new(Mutex::new(controls)), None);
    let mut state = Arena::new(controls, SEED);
    let agent = Agent::new_human(1u64);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: agent.clone() });
    let seat = state.seat_of(&1u64).expect("seated");
    let at = state.sim.tick() + 1;
    step(&logic, &mut state, LogicInput::AgentOps {
      source: agent.clone(),
      ops: vec![Op::Move { seq: 5, tick: at, dir: Dir8::E }],
    });
    step(&logic, &mut state, LogicInput::AgentOps {
      source: agent,
      ops: vec![Op::Move { seq: 2, tick: at, dir: Dir8::W }],
    });
    tick(&logic, &mut state);
    tick(&logic, &mut state);
    assert_eq!(state.sim.ships[seat].dir, Dir8::E);
  }

  #[test]
  fn a_departing_pilot_hands_the_seat_back() {
    let controls = quiet();
    let view = Arc::new(Mutex::new(HostView::default()));
    let logic = ArenaLogic::new(Arc::new(Mutex::new(controls)), Some(view.clone()));
    let mut state = Arena::new(controls, SEED);
    step(&logic, &mut state, LogicInput::AgentJoined { agent: Agent::new_human(1u64) });
    tick(&logic, &mut state);
    assert_eq!(view.lock().seats_taken, 1);
    step(&logic, &mut state, LogicInput::AgentLeft { agent_id: 1u64 });
    tick(&logic, &mut state);
    assert_eq!(view.lock().seats_taken, 0);
  }
}
