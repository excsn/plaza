//! The authority, which is authoritative over remarkably little.
//!
//! It owns the seed, the wave schedule, the tick a build lands on, and the
//! verdict on whether a build is legal. It does **not** own the enemies in the
//! sense the other examples' servers do: it simulates them, and so does every
//! client, from the same code and the same numbers. Nothing it computes about
//! an enemy is ever sent.
//!
//! That makes its regular output almost nothing, and it makes the one thing it
//! does send regularly, a digest, the load-bearing message. A server that sent
//! positions could be wrong about them and nobody would find out for long; this
//! one is *checked*, every half second, by every client independently.

use plaza_server_utils::{InputSchedule, InputWindow};

use crate::sim::protocol::{Op, ServerPolicy};
use crate::sim::rules::{self, Field, Quirks, StepEvents};
use crate::sim::types::*;

/// How long after a wave clears the next one begins, in ticks.
const GAP_TICKS: u64 = WAVE_GAP_MS / SIM_STEP_MS;
const PREP_TICKS: u64 = WAVE_PREP_MS / SIM_STEP_MS;

/// The schedule's window. See `want_build` for why it is not a judgement.
const WINDOW: InputWindow = InputWindow {
  max_late: 0,
  max_early: 400,
};

/// The seed the arena runs from.
///
/// One constant, and it is the entire content of a session's enemies. Changing
/// it changes every wave; a client that is handed a different one plays a
/// different game and finds out within half a second.
pub const WORLD_SEED: u64 = 0x5EED_DEFE_0001;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
  /// Before the first wave, and between waves. Towers may be built.
  Prep { until_tick: u64 },
  Running,
  /// The only ending. There is no last wave: they keep coming, and each one is
  /// harder than the one before, so every run ends here eventually.
  Lost,
}

/// What one tick produced, for the arena to dispatch.
#[derive(Clone, Debug, Default)]
pub struct Tickout {
  pub ops: Vec<Op>,
  pub events: StepEvents,
}

#[derive(Debug)]
pub struct Server {
  pub field: Field,
  pub seed: u64,
  pub phase: Phase,
  seats: Vec<bool>,
  schedule: InputSchedule<Build>,
  /// A wave announced but not yet laid out.
  ///
  /// Laid out at the *start* of its tick, exactly where a client lays it out,
  /// rather than at the end of the tick it was announced on. Those two are one
  /// tick apart, and one tick apart is a digest mismatch on the very first
  /// comparison of every wave.
  pending_wave: Option<(u32, u64)>,
  last_digest_tick: u64,

  pub builds_admitted: u64,
  pub builds_refused: u64,
  pub snapshots_sent: u64,
  pub digests_sent: u64,
  /// Bytes actually sent, and what the same session would have cost if the
  /// field went out at the send rate instead. The example's headline pair.
  pub bytes_sent: u64,
  pub bytes_if_streamed: u64,
}

impl Server {
  pub fn new(seats: usize, seed: u64) -> Self {
    Self {
      field: Field::default(),
      seed,
      phase: Phase::Prep { until_tick: PREP_TICKS },
      seats: vec![false; seats.clamp(1, 4)],
      schedule: InputSchedule::new(),
      pending_wave: None,
      last_digest_tick: 0,
      builds_admitted: 0,
      builds_refused: 0,
      snapshots_sent: 0,
      digests_sent: 0,
      bytes_sent: 0,
      bytes_if_streamed: 0,
    }
  }

  pub fn seats(&self) -> usize {
    self.seats.len()
  }

  pub fn tick(&self) -> u64 {
    self.field.tick
  }

  pub fn now_ms(&self) -> u64 {
    self.field.now_ms()
  }

  pub fn take_seat(&mut self, seat: usize) -> bool {
    match self.seats.get_mut(seat) {
      Some(taken) if !*taken => {
        *taken = true;
        true
      }
      _ => false,
    }
  }

  pub fn free_seat(&mut self) -> Option<usize> {
    self.seats.iter().position(|taken| !taken)
  }

  pub fn release_seat(&mut self, seat: usize) {
    if let Some(taken) = self.seats.get_mut(seat) {
      *taken = false;
    }
  }

  pub fn policy(&self, controls: &Controls) -> ServerPolicy {
    ServerPolicy {
      sync_hz: controls.sync_hz,
      playout_delay_ms: controls.playout_delay_ms,
      digest_interval_ms: DIGEST_INTERVAL_MS,
      sim_step_ms: SIM_STEP_MS,
      seats: self.seats.len(),
    }
  }

  pub fn welcome(&self, seat: usize, controls: &Controls) -> Op {
    Op::Welcome {
      player: seat as PlayerId,
      seed: self.seed,
      policy: self.policy(controls),
      field: Box::new(self.field.clone()),
      server_time_ms: self.now_ms(),
    }
  }

  /// The wave announcement a joiner missed, if one is outstanding.
  ///
  /// A client that connects during a prep phase never heard the announcement,
  /// and the field in its welcome does not contain the wave yet because the
  /// wave has not been laid out. Without this it would sit through the whole
  /// wave holding nothing, agreeing with no one.
  pub fn pending_wave_op(&self) -> Option<Op> {
    self.pending_wave.map(|(wave, start_tick)| Op::Wave { wave, start_tick })
  }

  pub fn snapshot(&mut self) -> Op {
    self.snapshots_sent += 1;
    Op::Snapshot {
      field: Box::new(self.field.clone()),
      server_time_ms: self.now_ms(),
    }
  }

  /// Takes a build request, and answers with the op every machine will apply.
  ///
  /// The tick is named here rather than by the client, and the reason is
  /// sharper than in the other examples: a client naming its own tick could
  /// name one in the past, and since every machine applies the op by
  /// *simulating* it, a build in the past is not a small unfairness, it is a
  /// state no other machine can reach.
  pub fn want_build(&mut self, seat: usize, seq: u64, cell: Cell, kind: TowerKind, upgrade: bool, controls: &Controls) -> Vec<Op> {
    let build = Build {
      player: seat as PlayerId,
      cell,
      kind,
      upgrade,
    };

    // Legality is checked against the field as it is *now*, which is not quite
    // the field the build will land on. That is deliberate: the alternative is
    // simulating forward to the landing tick to check, and a check that runs
    // the world forward is a second implementation of the world.
    let mut trial = self.field.clone();
    if !rules::apply_build(&mut trial, build) {
      self.builds_refused += 1;
      return vec![Op::Refused { seq }];
    }

    let at = self.tick() + (controls.playout_delay_ms / SIM_STEP_MS).max(1);
    // The window is wide open on the early side because the server named the
    // tick itself: the schedule is being used for its ordering and its
    // due-tick delivery, not to judge a claim a client made.
    self.schedule.submit(at, build, self.tick(), WINDOW);
    self.builds_admitted += 1;
    vec![Op::Ack { seq }, Op::Built { tick: at, build }]
  }

  /// Advances by whole ticks, and says what to send.
  pub fn advance(&mut self, dt_ms: u64, controls: &Controls) -> Tickout {
    let mut out = Tickout::default();
    let steps = (dt_ms / SIM_STEP_MS).min(16);
    for _ in 0..steps {
      self.step(controls, &mut out);
    }
    out
  }

  fn step(&mut self, controls: &Controls, out: &mut Tickout) {
    let next_tick = self.field.tick + 1;

    // Builds land on their named tick, before the step that tick performs, so
    // a tower placed for tick T fires on tick T everywhere.
    if let Some((wave, at)) = self.pending_wave
      && at == next_tick
    {
      self.pending_wave = None;
      self.begin_wave(wave, at);
      self.phase = Phase::Running;
    }

    let due: Vec<Build> = self.schedule.drain_due(next_tick).collect();
    for build in due {
      rules::apply_build(&mut self.field, build);
    }

    let events = rules::step(&mut self.field, Quirks::NONE);
    out.events.shots.extend(events.shots);
    out.events.kills.extend(events.kills);
    out.events.leaks.extend(events.leaks);

    self.advance_phase(out);
    self.publish_digest(out);

    // What the same tick would have cost if the field were streamed instead.
    // Counted every send interval rather than every tick, because a streaming
    // server would not send more often than it sends.
    let interval_ticks = (controls.sync_interval_ms() / SIM_STEP_MS).max(1);
    if self.field.tick % interval_ticks == 0 {
      self.bytes_if_streamed += crate::sim::protocol::field_cost(&self.field) as u64 * self.seats.len() as u64;
    }
  }

  fn advance_phase(&mut self, out: &mut Tickout) {
    if self.field.lives <= 0 {
      if self.phase != Phase::Lost {
        out.ops.push(Op::Over { wave: self.field.wave });
      }
      self.phase = Phase::Lost;
      return;
    }
    match self.phase {
      // Announced the moment the tick is known, which is a whole prep phase
      // ahead rather than one tick ahead. A client must have the announcement
      // *before* the tick it names, because laying a wave out late is a state
      // no other machine will ever hold; anything less than seconds of lead is
      // a design that only works on a fast link.
      Phase::Prep { until_tick } if self.pending_wave.is_none() => {
        let wave = self.field.wave + 1;
        self.pending_wave = Some((wave, until_tick));
        out.ops.push(Op::Wave {
          wave,
          start_tick: until_tick,
        });
      }
      // `pending_wave` guards this: between announcing a wave and laying it
      // out, the field holds the previous wave with nothing left in it, which
      // reads as cleared and would start the next one immediately.
      Phase::Running if self.pending_wave.is_none() && self.field.wave_cleared() => {
        self.phase = Phase::Prep {
          until_tick: self.field.tick + GAP_TICKS,
        };
      }
      _ => {}
    }
  }

  /// Lays out a wave. The **only** state this produces is derived from the seed
  /// and the wave number, which is why announcing those two is enough.
  pub fn begin_wave(&mut self, wave: u32, start_tick: u64) {
    self.field.wave = wave;
    self.field.pending = rules::wave_schedule(self.seed, wave, start_tick);
  }

  fn publish_digest(&mut self, out: &mut Tickout) {
    let interval = (DIGEST_INTERVAL_MS / SIM_STEP_MS).max(1);
    if self.field.tick.saturating_sub(self.last_digest_tick) < interval {
      return;
    }
    self.last_digest_tick = self.field.tick;
    self.digests_sent += 1;
    out.ops.push(Op::Digest {
      tick: self.field.tick,
      digest: self.field.digest(),
      enemies: self.field.enemies.len() as u32,
    });
  }

  /// Counts what actually went out, for the panel's comparison.
  pub fn charge_wire(&mut self, ops: &[Op], recipients: usize) {
    for op in ops {
      self.bytes_sent += crate::sim::protocol::wire_cost(op) as u64 * recipients as u64;
    }
  }
}

impl Clone for Server {
  /// `plaza` needs `Clone` for its state-query command. The input schedule is
  /// rebuilt empty, like the other examples: a half-drained queue is not worth
  /// copying, and nothing reads it from a snapshot.
  fn clone(&self) -> Self {
    Self {
      field: self.field.clone(),
      seed: self.seed,
      phase: self.phase,
      seats: self.seats.clone(),
      schedule: InputSchedule::new(),
      pending_wave: self.pending_wave,
      last_digest_tick: self.last_digest_tick,
      builds_admitted: self.builds_admitted,
      builds_refused: self.builds_refused,
      snapshots_sent: self.snapshots_sent,
      digests_sent: self.digests_sent,
      bytes_sent: self.bytes_sent,
      bytes_if_streamed: self.bytes_if_streamed,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn controls() -> Controls {
    Controls {
      latency_ms: 0,
      jitter_ms: 0,
      loss_pct: 0.0,
      players: 2,
      ..Controls::default()
    }
  }

  fn run(server: &mut Server, ms: u64, controls: &Controls) -> Vec<Op> {
    let mut ops = Vec::new();
    for _ in 0..(ms / SIM_STEP_MS) {
      ops.extend(server.advance(SIM_STEP_MS, controls).ops);
    }
    ops
  }

  #[test]
  fn a_wave_is_announced_as_two_integers_and_nothing_else() {
    let c = controls();
    let mut server = Server::new(2, 0xBEEF);
    let ops = run(&mut server, 20_000, &c);

    let waves = ops.iter().filter(|op| matches!(op, Op::Wave { .. })).count();
    assert_eq!(waves, 1, "one wave started");
    assert!(
      server.field.next_enemy > 10,
      "and it put real enemies on the board: {} spawned",
      server.field.next_enemy - 1
    );

    let describing_state = ops
      .iter()
      .filter(|op| matches!(op, Op::Snapshot { .. } | Op::Welcome { .. }))
      .count();
    assert_eq!(describing_state, 0, "and not one message described where anything is");
  }

  #[test]
  fn the_digest_goes_out_on_its_interval() {
    let c = controls();
    let mut server = Server::new(1, 1);
    let ops = run(&mut server, 5_000, &c);
    let digests = ops.iter().filter(|op| matches!(op, Op::Digest { .. })).count();
    let expected = 5_000 / DIGEST_INTERVAL_MS as usize;
    assert!(
      digests.abs_diff(expected) <= 1,
      "{digests} digests in five seconds, expected about {expected}"
    );
  }

  #[test]
  fn a_build_is_admitted_for_a_future_tick_and_refused_where_it_is_illegal() {
    let c = controls();
    let mut server = Server::new(1, 1);
    let now = server.tick();

    let ops = server.want_build(0, 1, Cell::new(4, 5), TowerKind::Arrow, false, &c);
    let landed = ops.iter().find_map(|op| match op {
      Op::Built { tick, .. } => Some(*tick),
      _ => None,
    });
    assert!(landed.expect("admitted") > now, "a build lands in the future, never in the past");

    let refused = server.want_build(0, 2, Cell::new(3, 2), TowerKind::Arrow, false, &c);
    assert!(matches!(refused.as_slice(), [Op::Refused { seq: 2 }]), "on the path");
    assert_eq!(server.field.towers.len(), 0, "and neither has been applied yet");
  }

  #[test]
  fn the_tower_appears_on_the_tick_the_op_named() {
    let c = controls();
    let mut server = Server::new(1, 1);
    let ops = server.want_build(0, 1, Cell::new(4, 5), TowerKind::Arrow, false, &c);
    let at = ops
      .iter()
      .find_map(|op| match op {
        Op::Built { tick, .. } => Some(*tick),
        _ => None,
      })
      .expect("admitted");

    while server.tick() < at - 1 {
      server.advance(SIM_STEP_MS, &c);
    }
    assert!(server.field.towers.is_empty(), "not before its tick");
    server.advance(SIM_STEP_MS, &c);
    assert_eq!(server.field.tick, at);
    assert_eq!(server.field.towers.len(), 1, "and exactly on it");
  }

  #[test]
  fn a_run_with_no_towers_is_eventually_lost() {
    let c = controls();
    let mut server = Server::new(1, 7);
    run(&mut server, 120_000, &c);
    assert_eq!(server.phase, Phase::Lost, "twenty lives and no defence");
  }

  #[test]
  fn the_only_ending_is_being_overrun_and_it_is_announced_once() {
    let c = controls();
    let mut server = Server::new(1, 7);
    let ops = run(&mut server, 120_000, &c);
    let overs: Vec<u32> = ops
      .iter()
      .filter_map(|op| match op {
        Op::Over { wave } => Some(*wave),
        _ => None,
      })
      .collect();
    assert_eq!(overs.len(), 1, "announced exactly once: {overs:?}");
    assert_eq!(server.phase, Phase::Lost);

    let after = run(&mut server, 30_000, &c);
    assert!(
      !after.iter().any(|op| matches!(op, Op::Wave { .. } | Op::Over { .. })),
      "and nothing further happens"
    );
  }

  #[test]
  fn the_waves_do_not_stop_and_each_one_is_harder() {
    let c = controls();
    let mut server = Server::new(1, 7);
    let mut reached = 0;
    for _ in 0..(600_000 / SIM_STEP_MS) {
      // Kept alive: this test is about the wave sequence, not about whether an
      // undefended map survives one.
      server.field.lives = STARTING_LIVES;
      server.advance(SIM_STEP_MS, &c);
      reached = reached.max(server.field.wave);
    }
    assert!(reached > 12, "the waves kept coming: reached {reached}");

    let early = EnemyKind::Grunt.hp(2);
    let late = EnemyKind::Grunt.hp(reached);
    assert!(late > early * 2, "and got harder: {early} hp at wave 2 against {late} at wave {reached}");
  }

  #[test]
  fn what_was_sent_is_a_fraction_of_what_streaming_would_have_cost() {
    let c = controls();
    let mut server = Server::new(2, 0xBEEF);
    for _ in 0..(40_000 / SIM_STEP_MS) {
      let out = server.advance(SIM_STEP_MS, &c);
      server.charge_wire(&out.ops, 2);
    }
    assert!(server.bytes_if_streamed > 0);
    assert!(
      server.bytes_sent * 20 < server.bytes_if_streamed,
      "sent {} bytes against {} streamed",
      server.bytes_sent,
      server.bytes_if_streamed
    );
  }
}
