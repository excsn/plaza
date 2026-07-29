//! A server and its clients in one process, with an impaired link between them.
//!
//! The measurement this harness exists for is a negative one, and it is the
//! sharpest version of it in the repository: **latency cannot affect a lap
//! time**. Not "barely", not "within a tolerance". The run happens entirely on
//! the machine driving it, so the link is not in the loop at all. The other
//! playgrounds spend their effort making latency cheap; here it is not on the
//! path.
//!
//! What the link does decide is when a ghost turns up, and how quickly a lie is
//! caught. Both are worth watching and neither touches the driving.

use plaza_client_utils::net_sim::{LatencyLink, Rng};

use crate::sim::client::Client;
use crate::sim::log::InputLog;
use crate::sim::protocol::Op;
use crate::sim::server::Server;
use crate::sim::types::*;

const IMPAIR_SEED: u64 = 0x6057_1A2E;

pub struct World {
  pub server: Server,
  pub clients: Vec<Client>,
  pub delivered: Vec<Vec<Op>>,
  down: Vec<LatencyLink<Op>>,
  up: LatencyLink<(usize, InputLog, u64)>,
  rng: Rng,
  clock_ms: u64,
}

impl World {
  pub fn new(controls: &Controls) -> Self {
    let count = controls.players.clamp(1, 4);
    let mut server = Server::new(count);
    let mut clients = Vec::new();
    for seat in 0..count {
      server.take_seat(seat);
      let mut client = Client::new(seat as PlayerId, Track::circuit(), server.rules_version);
      if let Op::Welcome { ghosts, .. } = server.welcome(seat) {
        client.on_ghosts(ghosts);
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
      clock_ms: 0,
    }
  }

  /// One simulation tick for everybody, and one turn of the wire.
  pub fn step(&mut self, controls: &Controls) {
    self.clock_ms += SIM_STEP_MS;
    self.server.advance(SIM_STEP_MS);
    let now = self.clock_ms;

    for row in self.delivered.iter_mut() {
      row.clear();
    }

    for (seat, client) in self.clients.iter_mut().enumerate() {
      for op in self.down[seat].drain_due(now) {
        self.delivered[seat].push(op.clone());
        match op {
          Op::Accepted { ghost, place } => client.on_accepted(*ghost, place),
          Op::Refused { why } => client.on_refused(why),
          Op::Welcome { ghosts, .. } => client.on_ghosts(ghosts),
          _ => {}
        }
      }
      if client.running {
        let input = crate::sim::rules::bot_input(client.racer(), &client.track, client.tick, 0);
        client.step(input, controls);
      }
      if let Some((log, claimed)) = client.take_submission() {
        self
          .up
          .send(now, (seat, log, claimed), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut self.rng);
      }
    }

    let due: Vec<(usize, InputLog, u64)> = self.up.drain_due(now);
    for (seat, log, claimed) in due {
      let answers = self.server.submit(seat, log, claimed);
      for op in answers {
        // An acceptance is everybody's business: a ghost is for racing. A
        // refusal is only the sender's.
        let broadcast = matches!(op, Op::Accepted { .. });
        for (to, link) in self.down.iter_mut().enumerate() {
          if broadcast || to == seat {
            link.send(now, op.clone(), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut self.rng);
          }
        }
      }
    }
  }

  pub fn start_all(&mut self) {
    self.start_all_as(Mode::Trial, TrackSize::Medium, 1);
  }

  pub fn start_all_as(&mut self, mode: Mode, size: TrackSize, field: usize) {
    for client in self.clients.iter_mut() {
      client.restart_as(mode, size, field);
    }
  }

  pub fn run(&mut self, ms: u64, controls: &Controls) {
    for _ in 0..(ms / SIM_STEP_MS) {
      self.step(controls);
    }
  }
}

/// The stand-in for a player.
///
/// The same function the CPU field drives with, which lives in `rules` because
/// in a race it is part of what a log reproduces. A harness that had its own
/// copy would be testing a driver nobody plays against.
pub use crate::sim::rules::bot_input as autopilot;

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

  #[test]
  fn latency_cannot_change_a_lap_time() {
    let mut times = Vec::new();
    for latency in [0u64, 80, 250, 400] {
      let c = Controls {
        latency_ms: latency,
        jitter_ms: latency / 3,
        ..quiet()
      };
      let mut world = World::new(&c);
      world.start_all();
      world.run(60_000, &c);
      times.push(world.clients[0].finished_ms.expect("it finished"));
    }
    assert!(times.windows(2).all(|w| w[0] == w[1]), "the same drive gave {times:?}");
  }

  #[test]
  fn a_verified_run_reaches_the_other_client_as_a_ghost() {
    let c = quiet();
    let mut world = World::new(&c);
    world.start_all();
    world.run(60_000, &c);

    assert_eq!(world.server.accepted, 2, "both clients submitted a real run");
    assert!(!world.clients[1].ghosts.is_empty(), "and the other one got a ghost");
    let ghost = &world.clients[1].ghosts[0].ghost;
    assert_eq!(
      crate::sim::log::replay(&ghost.log, &world.clients[1].track).time_ms(),
      Some(ghost.time_ms),
      "which replays, here, to the time it was recorded at"
    );
  }

  #[test]
  fn a_lie_is_caught_wherever_it_is_sent_from() {
    let c = Controls {
      cheat: true,
      latency_ms: 120,
      jitter_ms: 30,
      ..quiet()
    };
    let mut world = World::new(&c);
    world.start_all();
    world.run(60_000, &c);

    assert_eq!(world.server.accepted, 0, "nothing was recorded");
    assert_eq!(world.server.refused, 2, "both lies were refused");
    assert!(world.clients[0].last_refusal.is_some(), "and the sender was told");
    assert!(world.server.board.is_empty());
  }

  #[test]
  fn a_lost_submission_costs_the_run_rather_than_the_board() {
    // There is no retry here, deliberately. A dropped submission is a lap
    // nobody recorded, which is a disappointment and not a corruption: the
    // board still holds only runs that were verified.
    let c = Controls {
      loss_pct: 100.0,
      latency_ms: 60,
      ..quiet()
    };
    let mut world = World::new(&c);
    world.start_all();
    world.run(60_000, &c);

    assert!(world.clients[0].finished_ms.is_some(), "the run happened");
    assert_eq!(world.server.submissions, 0, "and none of it arrived");
    assert!(world.server.board.is_empty());
  }

  #[test]
  fn nothing_but_logs_ever_crosses_the_wire() {
    let c = quiet();
    let mut world = World::new(&c);
    world.start_all();

    let mut ghosts = 0;
    let mut path_cost = 0;
    let mut log_cost = 0;
    for _ in 0..(60_000 / SIM_STEP_MS) {
      world.step(&c);
      for ops in &world.delivered {
        for op in ops {
          if let Op::Accepted { ghost, .. } = op {
            ghosts += 1;
            log_cost += ghost.log.wire_cost();
            path_cost += ghost.log.path_cost();
          }
        }
      }
    }
    assert!(ghosts > 0, "a ghost crossed");
    assert!(
      log_cost * 8 < path_cost,
      "{log_cost} bytes of inputs against {path_cost} of positions"
    );
  }
}
