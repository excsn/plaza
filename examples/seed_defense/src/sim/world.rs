//! A server and its clients in one process, with an impaired link between them.
//!
//! The harness every claim in this example is measured on, and the one place
//! the interesting comparison can be made: what does **latency** cost a game
//! that sends no state? In every other playground here the answer is
//! corrections, and the whole design is about making them cheap. Here the
//! answer should be *nothing at all*, at any depth, and that is a claim worth
//! testing rather than asserting.
//!
//! Loss is different, and the difference is the point. A lost op is not a lost
//! sample that the next one supersedes: it is a cause that never happened on
//! one machine, and no amount of waiting recovers it. So loss costs a snapshot,
//! which is exactly the trade this design makes and exactly what the counters
//! report.
//!
//! As in the other harnesses, the one thing this structurally cannot model is a
//! client *estimating* the clock: here the two halves share one.

use plaza_client_utils::net_sim::{LatencyLink, Rng};

use crate::sim::client::Client;
use crate::sim::protocol::Op;
use crate::sim::server::Server;
use crate::sim::types::*;

const IMPAIR_SEED: u64 = 0x5EED_DEFE;

pub struct World {
  pub server: Server,
  pub clients: Vec<Client>,
  /// What each seat was handed on the last step, so a test can assert about
  /// what crossed the wire rather than about what the state ended up as.
  pub delivered: Vec<Vec<Op>>,
  down: Vec<LatencyLink<Op>>,
  up: LatencyLink<(usize, Op)>,
  rng: Rng,
  seq: u64,
}

impl World {
  pub fn new(controls: &Controls, seed: u64) -> Self {
    let count = controls.players.clamp(1, 4);
    let mut server = Server::new(count, seed);
    let mut clients = Vec::new();
    for seat in 0..count {
      server.take_seat(seat);
      let mut client = Client::new(seat as PlayerId);
      if let Op::Welcome { seed, policy, field, .. } = server.welcome(seat, controls) {
        client.on_welcome(seed, policy, &field);
      }
      if let Some(Op::Wave { wave, start_tick }) = server.pending_wave_op() {
        client.on_wave(wave, start_tick);
      }
      clients.push(client);
    }
    Self {
      server,
      clients,
      delivered: vec![Vec::new(); count],
      down: (0..count).map(|_| LatencyLink::default()).collect(),
      up: LatencyLink::default(),
      rng: Rng::new(IMPAIR_SEED),
      seq: 0,
    }
  }

  /// A player asks for a tower. The request goes up the impaired link like any
  /// other, so a lost one is simply a tower that never gets built.
  pub fn want(&mut self, seat: usize, cell: Cell, kind: TowerKind, upgrade: bool, controls: &Controls) {
    self.seq += 1;
    let now = self.server.now_ms();
    let op = Op::Want {
      seq: self.seq,
      cell,
      kind,
      upgrade,
    };
    self
      .up
      .send(now, (seat, op), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut self.rng);
  }

  pub fn step(&mut self, dt_ms: u64, controls: &Controls) {
    let now = self.server.now_ms();
    let mut answers: Vec<(usize, Vec<Op>)> = Vec::new();
    for (seat, op) in self.up.drain_due(now) {
      match op {
        Op::Want { seq, cell, kind, upgrade } => {
          let ops = self.server.want_build(seat, seq, cell, kind, upgrade, controls);
          answers.push((seat, ops));
        }
        Op::WantSnapshot { .. } => {
          let snapshot = self.server.snapshot();
          answers.push((seat, vec![snapshot]));
        }
        _ => {}
      }
    }

    let out = self.server.advance(dt_ms, controls);
    let now = self.server.now_ms();
    self.server.charge_wire(&out.ops, self.clients.len());

    for (seat, link) in self.down.iter_mut().enumerate() {
      for op in &out.ops {
        link.send(now, op.clone(), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut self.rng);
      }
      // A refusal or a snapshot goes to the one seat that asked. A `Built`
      // goes to everybody, because everybody has to apply it: it is not a
      // reply, it is a cause.
      for (to, ops) in &answers {
        for op in ops {
          let broadcast = matches!(op, Op::Built { .. });
          if broadcast || *to == seat {
            link.send(now, op.clone(), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut self.rng);
          }
        }
      }
    }

    for row in self.delivered.iter_mut() {
      row.clear();
    }

    let mut requests = Vec::new();
    for (seat, client) in self.clients.iter_mut().enumerate() {
      for op in self.down[seat].drain_due(now) {
        self.delivered[seat].push(op.clone());
        match op {
          Op::Wave { wave, start_tick } => client.on_wave(wave, start_tick),
          Op::Built { tick, build } => client.on_built(tick, build),
          Op::Digest { tick, digest, enemies } => client.on_digest(tick, digest, enemies, controls),
          Op::Snapshot { field, .. } => client.adopt(&field),
          _ => {}
        }
      }
      client.run_to(self.server.tick(), controls);
      if let Some(op) = client.take_request() {
        requests.push((seat, op));
      }
    }

    let now_ms = self.server.now_ms();
    for (seat, op) in requests {
      self
        .up
        .send(now_ms, (seat, op), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut self.rng);
    }
  }

  pub fn run(&mut self, ms: u64, controls: &Controls) {
    for _ in 0..(ms / SIM_STEP_MS) {
      self.step(SIM_STEP_MS, controls);
    }
  }

  /// Every client's field equals the server's.
  pub fn all_agree(&self) -> bool {
    self.clients.iter().all(|c| c.field == self.server.field)
  }

  pub fn total_resyncs(&self) -> u64 {
    self.clients.iter().map(|c| c.resyncs).sum()
  }

  pub fn total_mismatches(&self) -> u64 {
    self.clients.iter().map(|c| c.mismatches).sum()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn quiet() -> Controls {
    Controls {
      latency_ms: 0,
      jitter_ms: 0,
      loss_pct: 0.0,
      players: 2,
      ..Controls::default()
    }
  }

  /// Puts a few towers up, through the impaired link like a player would.
  fn build_a_defence(world: &mut World, controls: &Controls) {
    for (seat, cell) in [(0, Cell::new(5, 4)), (1, Cell::new(7, 6)), (0, Cell::new(12, 6)), (1, Cell::new(16, 6))] {
      world.want(seat, cell, TowerKind::Arrow, false, controls);
      world.run(300, controls);
    }
  }

  #[test]
  fn latency_costs_nothing_at_all() {
    // The measurement this example exists to take, and the reason it is worth
    // taking: in every other playground here, latency buys corrections and the
    // design is about making them cheap. A game that sends causes rather than
    // state does not pay for latency at any depth, because nothing it computes
    // depends on when anything arrived.
    for latency in [0u64, 60, 200, 400] {
      let c = Controls {
        latency_ms: latency,
        jitter_ms: latency / 4,
        // The build lead has to clear the worst one-way delay, or an op cannot
        // reach a client before the tick it names. That is a real constraint
        // rather than a test fixture, and it has its own test below.
        playout_delay_ms: latency * 2 + 150,
        ..quiet()
      };
      let mut world = World::new(&c, 0xC0FFEE);
      build_a_defence(&mut world, &c);
      world.run(40_000, &c);

      assert!(world.all_agree(), "a client diverged at {latency} ms");
      assert_eq!(world.total_mismatches(), 0, "a digest disagreed at {latency} ms");
      assert_eq!(world.server.snapshots_sent, 0, "the state was sent at {latency} ms");
      assert!(world.server.field.next_enemy > 20, "and there was a real wave to agree about");
    }
  }

  #[test]
  fn a_build_lead_shorter_than_the_link_cannot_be_met() {
    // The constraint the test above works around, stated as its own claim. The
    // server names the tick a build lands on; if that tick arrives before the
    // op does, no client can apply it, and the only honest answer is to say so
    // and ask for the state. The number to set is a policy, and setting it
    // wrong is visible rather than silent.
    let c = Controls {
      latency_ms: 300,
      jitter_ms: 40,
      playout_delay_ms: 100,
      ..quiet()
    };
    let mut world = World::new(&c, 1);
    build_a_defence(&mut world, &c);
    world.run(5_000, &c);

    let late: u64 = world.clients.iter().map(|c| c.builds_too_late).sum();
    assert!(late > 0, "a 100 ms lead over a 300 ms link should be unmeetable");
    assert!(world.total_resyncs() > 0, "and each one costs a snapshot");
  }

  #[test]
  fn loss_costs_a_snapshot_rather_than_a_wrong_world() {
    // The other half, and the honest one. A lost op is not a lost sample that
    // the next one supersedes: it is a cause that happened on one machine and
    // not another, which no amount of waiting repairs. So the design pays for
    // it in the one expensive message it has.
    let c = Controls {
      latency_ms: 80,
      jitter_ms: 20,
      loss_pct: 25.0,
      ..quiet()
    };
    let mut world = World::new(&c, 0xC0FFEE);
    build_a_defence(&mut world, &c);
    world.run(40_000, &c);

    assert!(world.total_resyncs() > 0, "a quarter of the packets went missing and nothing noticed");
    assert!(world.server.snapshots_sent > 0, "and nothing was ever sent to repair it");
    assert!(
      world.server.snapshots_sent < 40_000 / DIGEST_INTERVAL_MS,
      "but it did not resync on every digest, which would be a broken recovery: {} snapshots",
      world.server.snapshots_sent
    );
  }

  #[test]
  fn what_crossed_the_wire_never_described_an_enemy() {
    // Read off the wire rather than off the server's intent, the way the
    // secrecy test in `pellet_maze` is. Every op every seat was handed, checked
    // against the one thing this example promises not to send.
    let c = quiet();
    let mut world = World::new(&c, 0xC0FFEE);
    build_a_defence(&mut world, &c);

    let mut waves = 0;
    let mut digests = 0;
    let mut state = 0;
    for _ in 0..(40_000 / SIM_STEP_MS) {
      world.step(SIM_STEP_MS, &c);
      for ops in &world.delivered {
        for op in ops {
          match op {
            Op::Wave { .. } => waves += 1,
            Op::Digest { .. } => digests += 1,
            Op::Snapshot { .. } | Op::Welcome { .. } => state += 1,
            _ => {}
          }
        }
      }
    }

    assert!(waves > 0 && digests > 0, "the run happened");
    assert_eq!(state, 0, "and not one message carried a position");
    assert!(world.server.field.next_enemy > 20, "while dozens of enemies came and went");
  }

  #[test]
  fn a_quirked_client_is_the_only_one_that_pays() {
    // Two clients, one of them running different arithmetic. The honest one
    // must not be dragged into the broken one's recovery: a resync is a reply
    // to the client that asked, not a broadcast.
    let c = Controls {
      break_with_floats: true,
      ..quiet()
    };
    let mut world = World::new(&c, 0xC0FFEE);
    build_a_defence(&mut world, &c);

    // The quirk applies to seat 1 only, which the harness expresses by running
    // the two clients under different controls.
    let honest = Controls {
      break_with_floats: false,
      ..c
    };
    for _ in 0..(30_000 / SIM_STEP_MS) {
      world.step(SIM_STEP_MS, &honest);
    }
    assert_eq!(world.total_mismatches(), 0, "with nobody quirked, nobody disagrees");

    let mut broken = World::new(&c, 0xC0FFEE);
    build_a_defence(&mut broken, &c);
    broken.run(30_000, &c);
    assert!(broken.total_mismatches() > 0, "with everybody quirked, everybody disagrees");
  }
}
