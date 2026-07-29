//! The client, which runs the whole game and is told almost nothing.
//!
//! In every other playground here the client's simulation is a *prediction*: a
//! guess about the near future, continuously corrected by a server that knows
//! better. This one is not a prediction at all. It is the same simulation, run
//! from the same inputs, and its correctness does not decay with time or with
//! latency. A client on a 400 ms link produces exactly the same wave as the
//! server, because nothing it computes depends on when it heard anything.
//!
//! What it does depend on is applying every op **on the tick that op names**.
//! That is the one fragile point, and it is fragile in a way worth watching:
//! an op that arrives after its tick has passed cannot be applied late, because
//! late means simulating a different history. So a client in that position has
//! only one honest move, which is to admit it and ask for the state.
//!
//! Hence the two counters this file exists to produce: **mismatches**, meaning
//! a digest disagreed, and **resyncs**, meaning the client gave up and asked.
//! An example with zero of the first is an example that has proved nothing, so
//! the panel can cause both on demand.

use std::collections::VecDeque;

use crate::sim::protocol::{Op, ServerPolicy};
use crate::sim::rules::{self, Field, Quirks, StepEvents};
use crate::sim::types::*;

/// Why the client last asked for a snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResyncReason {
  /// A digest disagreed with the server's.
  Mismatch,
  /// A build op named a tick this client had already simulated past.
  BuildTooLate,
  /// The server's digest named a tick this client no longer remembers, which
  /// means it has fallen far enough behind that comparison is meaningless.
  Forgotten,
}

pub struct Client {
  pub me: PlayerId,
  pub field: Field,
  pub seed: u64,
  pub policy: ServerPolicy,
  /// Builds waiting for the tick they named.
  pending_builds: Vec<(u64, Build)>,
  /// Waves waiting for the tick they begin on. A wave announcement can arrive
  /// before its start tick, which is the normal case and the point of naming a
  /// tick at all.
  pending_waves: Vec<(u32, u64)>,
  /// This client's own digest at each recent tick, so a server digest that
  /// arrives a few hundred milliseconds later can still be answered.
  history: VecDeque<(u64, u64)>,
  /// Digests naming a tick this client has not simulated yet.
  ///
  /// They arrive early whenever the client is running behind the server, which
  /// on a real link is most of the time. Comparing one against the newest state
  /// instead of waiting would report a mismatch on every message: the client
  /// would not be wrong, it would be *earlier*.
  incoming: VecDeque<(u64, u64, u32)>,

  pub ticks_run: u64,
  pub mismatches: u64,
  pub resyncs: u64,
  pub builds_too_late: u64,
  pub last_reason: Option<ResyncReason>,
  /// The wave the line broke on, once it has.
  pub over: Option<u32>,
  /// The tick a mismatch was last seen at, and what each side held.
  pub last_mismatch: Option<(u64, u64, u64, u32, u32)>,
  /// Set when a snapshot is wanted. The net layer drains it.
  wants_snapshot: Option<(u64, u64)>,
  pub events: StepEvents,
}

impl Client {
  pub fn new(me: PlayerId) -> Self {
    Self {
      me,
      field: Field::default(),
      seed: 0,
      policy: ServerPolicy {
        sync_hz: 10,
        playout_delay_ms: 120,
        digest_interval_ms: DIGEST_INTERVAL_MS,
        sim_step_ms: SIM_STEP_MS,
        seats: 1,
      },
      pending_builds: Vec::new(),
      pending_waves: Vec::new(),
      history: VecDeque::new(),
      incoming: VecDeque::new(),
      ticks_run: 0,
      mismatches: 0,
      resyncs: 0,
      builds_too_late: 0,
      last_reason: None,
      over: None,
      last_mismatch: None,
      wants_snapshot: None,
      events: StepEvents::default(),
    }
  }

  pub fn tick(&self) -> u64 {
    self.field.tick
  }

  pub fn now_ms(&self) -> u64 {
    self.field.now_ms()
  }

  pub fn on_welcome(&mut self, seed: u64, policy: ServerPolicy, field: &Field) {
    self.seed = seed;
    self.policy = policy;
    self.adopt(field);
  }

  /// Replaces the whole field. The expensive path, and the only one that ever
  /// moves state across the wire.
  pub fn adopt(&mut self, field: &Field) {
    self.field = field.clone();
    self.history.clear();
    self.incoming.clear();
    self.pending_builds.retain(|(at, _)| *at > self.field.tick);
    self.pending_waves.retain(|(_, at)| *at > self.field.tick);
    self.wants_snapshot = None;
  }

  /// The next wave and the tick it begins on, if one has been announced and
  /// not yet laid out.
  ///
  /// The countdown between waves comes from here rather than from a message of
  /// its own: the announcement already names the tick, and a separate timer
  /// would be a second opinion about the same moment.
  pub fn next_wave(&self) -> Option<(u32, u64)> {
    self.pending_waves.iter().min_by_key(|(_, at)| *at).copied()
  }

  pub fn on_wave(&mut self, wave: u32, start_tick: u64) {
    if start_tick <= self.field.tick {
      // The wave was announced for a tick already simulated. Laying it out now
      // would spawn its first enemies late and every machine would then hold a
      // different set. There is no local repair for that.
      self.ask_for_snapshot(ResyncReason::BuildTooLate);
      return;
    }
    self.pending_waves.push((wave, start_tick));
  }

  pub fn on_built(&mut self, tick: u64, build: Build) {
    if tick <= self.field.tick {
      self.builds_too_late += 1;
      self.ask_for_snapshot(ResyncReason::BuildTooLate);
      return;
    }
    self.pending_builds.push((tick, build));
  }

  /// Takes a server digest. **Held until this client has simulated that tick.**
  ///
  /// The comparison is made at the server's tick, never at the newest one, for
  /// the same reason the lattice examples compare a correction against what the
  /// client believed at the frame's own timestamp: a client running behind is
  /// not wrong, it is earlier, and comparing across the gap reports a mismatch
  /// on every single message.
  pub fn on_digest(&mut self, tick: u64, digest: u64, enemies: u32, controls: &Controls) {
    if !controls.digest_checks {
      return;
    }
    self.incoming.push_back((tick, digest, enemies));
    while self.incoming.len() > DIGEST_MEMORY {
      self.incoming.pop_front();
    }
    self.compare_ready(controls);
  }

  /// Compares every held digest whose tick this client has now reached.
  fn compare_ready(&mut self, controls: &Controls) {
    while self.incoming.front().is_some_and(|(tick, _, _)| *tick <= self.field.tick) {
      let (tick, theirs, enemies) = self.incoming.pop_front().expect("just checked");
      let Some(mine) = self.digest_at(tick) else {
        // Simulated, then forgotten: this client is far enough behind that the
        // tick has aged out of its memory, so it cannot answer and cannot know
        // whether it agreed.
        self.ask_for_snapshot(ResyncReason::Forgotten);
        continue;
      };
      if mine == theirs {
        continue;
      }
      self.mismatches += 1;
      self.last_mismatch = Some((tick, mine, theirs, self.field.enemies.len() as u32, enemies));
      if controls.resync_on_mismatch {
        self.ask_for_snapshot(ResyncReason::Mismatch);
      }
    }
  }

  fn ask_for_snapshot(&mut self, reason: ResyncReason) {
    self.last_reason = Some(reason);
    self.resyncs += 1;
    self.wants_snapshot = Some((self.field.tick, self.field.digest()));
  }

  /// What the net layer should send, if anything.
  pub fn take_request(&mut self) -> Option<Op> {
    self.wants_snapshot.take().map(|(at_tick, mine)| Op::WantSnapshot { at_tick, mine })
  }

  /// Advances the local simulation to `target_tick`.
  ///
  /// Driven by a tick rather than by elapsed time, and the reason is the one
  /// `bomb_grid` paid four bugs to learn: a simulation both sides run must
  /// advance in the same quantum, and a caller must not be able to influence
  /// how fast it goes. Here the stakes are higher still, because there is no
  /// correction to hide a difference.
  pub fn run_to(&mut self, target_tick: u64, controls: &Controls) {
    if !controls.simulate_locally {
      return;
    }
    let quirks = Quirks {
      floats: controls.break_with_floats,
      target_order: controls.break_target_order,
      slow_rounding: controls.break_slow_rounding,
    };
    self.events.shots.clear();
    self.events.kills.clear();
    self.events.leaks.clear();

    // Bounded, so a client that has been asleep does not spend a whole frame
    // catching up and then miss the next one. Falling further behind than this
    // is what the resync is for.
    let mut budget = 400;
    while self.field.tick < target_tick && budget > 0 {
      budget -= 1;
      let next = self.field.tick + 1;

      for (wave, _) in self.pending_waves.extract_if(.., |(_, at)| *at == next).collect::<Vec<_>>() {
        self.field.wave = wave;
        self.field.pending = rules::wave_schedule(self.seed, wave, next);
      }
      for (_, build) in self.pending_builds.extract_if(.., |(at, _)| *at == next).collect::<Vec<_>>() {
        rules::apply_build(&mut self.field, build);
      }

      let events = rules::step(&mut self.field, quirks);
      self.events.shots.extend(events.shots);
      self.events.kills.extend(events.kills);
      self.events.leaks.extend(events.leaks);
      self.ticks_run += 1;

      self.history.push_back((self.field.tick, self.field.digest()));
      while self.history.len() > DIGEST_MEMORY {
        self.history.pop_front();
      }
    }
    self.compare_ready(controls);
  }

  /// The digest this client holds for a tick, if it still remembers it.
  pub fn digest_at(&self, tick: u64) -> Option<u64> {
    self.history.iter().rev().find(|(t, _)| *t == tick).map(|(_, d)| *d)
  }

  /// Whether a build is worth asking for, checked locally so a player is not
  /// told "no" by a round trip. It is **not** applied locally: see the module
  /// note on why a client never simulates an op the server has not scheduled.
  pub fn could_build(&self, cell: Cell, kind: TowerKind, upgrade: bool) -> bool {
    let mut trial = self.field.clone();
    rules::apply_build(
      &mut trial,
      Build {
        player: self.me,
        cell,
        kind,
        upgrade,
      },
    )
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::sim::server::Server;

  fn controls() -> Controls {
    Controls {
      latency_ms: 0,
      jitter_ms: 0,
      loss_pct: 0.0,
      players: 2,
      ..Controls::default()
    }
  }

  /// A server and a client wired directly together, with no link at all: the
  /// smallest thing that can show the two staying identical.
  fn paired(seed: u64, c: &Controls) -> (Server, Client) {
    let mut server = Server::new(2, seed);
    server.take_seat(0);
    let mut client = Client::new(0);
    match server.welcome(0, c) {
      Op::Welcome { seed, policy, field, .. } => client.on_welcome(seed, policy, &field),
      _ => unreachable!(),
    }
    (server, client)
  }

  fn pump(server: &mut Server, client: &mut Client, ms: u64, c: &Controls) {
    for _ in 0..(ms / SIM_STEP_MS) {
      let out = server.advance(SIM_STEP_MS, c);
      for op in out.ops {
        match op {
          Op::Wave { wave, start_tick } => client.on_wave(wave, start_tick),
          Op::Built { tick, build } => client.on_built(tick, build),
          Op::Digest { tick, digest, enemies } => client.on_digest(tick, digest, enemies, c),
          _ => {}
        }
      }
      client.run_to(server.tick(), c);
    }
  }

  #[test]
  fn a_client_told_only_the_seed_reproduces_the_whole_wave() {
    let c = controls();
    let (mut server, mut client) = paired(0xC0FFEE, &c);
    pump(&mut server, &mut client, 30_000, &c);

    assert!(client.field.next_enemy > 5, "there was a wave to reproduce");
    assert_eq!(client.field, server.field, "and it was reproduced exactly");
    assert_eq!(client.mismatches, 0);
    assert_eq!(client.resyncs, 0);
  }

  #[test]
  fn a_build_lands_on_the_same_tick_on_both_sides() {
    let c = controls();
    let (mut server, mut client) = paired(1, &c);
    pump(&mut server, &mut client, 1_000, &c);

    for op in server.want_build(0, 1, Cell::new(4, 5), TowerKind::Arrow, false, &c) {
      if let Op::Built { tick, build } = op {
        client.on_built(tick, build);
      }
    }
    pump(&mut server, &mut client, 2_000, &c);

    assert_eq!(server.field.towers.len(), 1);
    assert_eq!(client.field.towers, server.field.towers);
    assert_eq!(client.field.gold, server.field.gold, "and both paid for it");
  }

  #[test]
  fn a_quirked_client_is_caught_by_the_digest() {
    let c = Controls {
      break_with_floats: true,
      resync_on_mismatch: false,
      ..controls()
    };
    let (mut server, mut client) = paired(0xC0FFEE, &c);
    for cell in [Cell::new(5, 4), Cell::new(7, 6), Cell::new(12, 6)] {
      for op in server.want_build(0, 1, cell, TowerKind::Arrow, false, &c) {
        if let Op::Built { tick, build } = op {
          client.on_built(tick, build);
        }
      }
    }
    pump(&mut server, &mut client, 60_000, &c);

    assert!(client.mismatches > 0, "the digest never noticed a client with different arithmetic");
    assert_ne!(client.field, server.field, "and there really was something to notice");
  }

  #[test]
  fn a_resync_puts_a_diverged_client_back_on_the_rails() {
    let c = Controls {
      break_with_floats: true,
      resync_on_mismatch: true,
      ..controls()
    };
    let (mut server, mut client) = paired(0xC0FFEE, &c);
    for cell in [Cell::new(5, 4), Cell::new(7, 6), Cell::new(12, 6)] {
      for op in server.want_build(0, 1, cell, TowerKind::Arrow, false, &c) {
        if let Op::Built { tick, build } = op {
          client.on_built(tick, build);
        }
      }
    }

    let mut resyncs = 0;
    for _ in 0..(60_000 / SIM_STEP_MS) {
      let out = server.advance(SIM_STEP_MS, &c);
      for op in out.ops {
        match op {
          Op::Wave { wave, start_tick } => client.on_wave(wave, start_tick),
          Op::Built { tick, build } => client.on_built(tick, build),
          Op::Digest { tick, digest, enemies } => client.on_digest(tick, digest, enemies, &c),
          _ => {}
        }
      }
      client.run_to(server.tick(), &c);
      if client.take_request().is_some() {
        resyncs += 1;
        let snapshot = server.snapshot();
        if let Op::Snapshot { field, .. } = snapshot {
          client.adopt(&field);
        }
      }
    }

    assert!(resyncs > 0, "it never asked");
    // The state right after a snapshot is by construction identical, so the
    // interesting assertion is the one about cost: a broken client is expensive
    // rather than wrong, which is the trade the whole design makes.
    assert!(
      resyncs < 60_000 / DIGEST_INTERVAL_MS as usize,
      "it resynced on every single digest, which is a broken recovery rather than a working one"
    );
  }

  #[test]
  fn a_build_that_arrives_after_its_tick_is_not_applied_late() {
    let c = controls();
    let (mut server, mut client) = paired(1, &c);
    pump(&mut server, &mut client, 2_000, &c);

    let build = Build {
      player: 0,
      cell: Cell::new(4, 5),
      kind: TowerKind::Arrow,
      upgrade: false,
    };
    let towers = client.field.towers.len();
    client.on_built(client.field.tick - 4, build);

    assert_eq!(client.field.towers.len(), towers, "it was not applied");
    assert_eq!(client.builds_too_late, 1);
    assert!(client.take_request().is_some(), "and it asked for the state instead");
  }
}
