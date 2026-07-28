//! Server and clients in one process, with an impaired link between them.
//!
//! The harness every claim in this example is measured on. It is not the
//! networked build: it stands in for the wire so a rule can be tested without a
//! socket, and so a measurement can be repeated exactly rather than played.
//!
//! The impairment is [`plaza_client_utils::net_sim::LatencyLink`], which is
//! ordered by default and is the same one the networked host puts on its real
//! outbound path. Two copies of a delay queue that agree today are a
//! disagreement waiting to happen, and this repository has paid for that once
//! already.

use plaza_client_utils::net_sim::{LatencyLink, Rng};

use crate::sim::client::Client;
use crate::sim::protocol::{Frame, Intent, Op};
use crate::sim::server::Server;
use crate::sim::types::*;

/// The seed for the impairment jitter, fixed so a run is reproducible.
const IMPAIR_SEED: u64 = 0x0BB1_5EED;

/// One process holding the authority and every client, with a link between.
pub struct World {
  pub server: Server,
  pub clients: Vec<Client>,
  /// Downstream, one queue per client.
  down: Vec<LatencyLink<Op>>,
  /// Upstream: `(seat, tick, intent)` held for the same delay.
  up: LatencyLink<(usize, u64, Intent)>,
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
      client.on_round(&server.round_start());
      client.set_render_delay(controls.render_delay_ms);
      clients.push(client);
    }
    Self {
      server,
      clients,
      down: (0..count).map(|_| LatencyLink::default()).collect(),
      up: LatencyLink::default(),
      rng: Rng::new(IMPAIR_SEED),
      seq: 0,
    }
  }

  /// Offers an input from `seat`, aimed the way a real client aims one: at the
  /// tick it will be executed on, which is now plus the playout depth.
  pub fn input(&mut self, seat: usize, intent: Intent, controls: &Controls) {
    self.seq += 1;
    let now = self.server.now_ms();
    let tick = (now + controls.playout_delay_ms) / SIM_STEP_MS;
    if let Some(client) = self.clients.get_mut(seat) {
      client.schedule_input(self.seq, tick, intent, now);
    }
    self.up.send(now, (seat, tick, intent), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut self.rng);
  }

  /// One simulation step for everybody.
  pub fn step(&mut self, dt_ms: u64, controls: &Controls) {
    let now = self.server.now_ms();
    for (seat, tick, intent) in self.up.drain_due(now) {
      self.server.submit(seat, tick, intent, controls);
    }

    let out = self.server.advance(dt_ms, controls);
    let now = self.server.now_ms();

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

    for link in &mut self.down {
      for op in &outbound {
        link.send(now, op.clone(), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut self.rng);
      }
    }

    for (seat, client) in self.clients.iter_mut().enumerate() {
      // Each client's own estimate of server-now. In this harness the clock is
      // shared, which is exactly what the networked build cannot do; that is
      // the one thing this harness cannot measure.
      client.tick(now, controls);
      for op in self.down[seat].drain_due(now) {
        match op {
          Op::Frame(frame) => client.on_frame(&frame, controls),
          Op::Blast(blast) => client.on_blast(&blast),
          Op::Round(round) => client.on_round(&round),
          Op::RoundOver { .. } => client.set_paused(true),
          _ => {}
        }
      }
    }
  }

  /// Runs for `ms`, in simulation steps.
  pub fn run(&mut self, ms: u64, controls: &Controls) {
    for _ in 0..(ms / SIM_STEP_MS) {
      self.step(SIM_STEP_MS, controls);
    }
  }

  /// Total snaps across every client: the headline number.
  pub fn total_snaps(&self) -> u64 {
    self.clients.iter().map(|c| c.snaps).sum()
  }

  /// What one client believes about its own cell, against the truth.
  pub fn disagreement(&self, seat: usize) -> Option<u16> {
    let client = self.clients.get(seat)?;
    let truth = self.server.players.get(seat)?;
    Some(client.my_player().cell.distance(truth.cell))
  }

  /// A crude frame, for a test that wants to hand a client the truth directly.
  pub fn truth_frame(&self) -> Frame {
    self.server.frame()
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
      bots: false,
      players: 2,
      sync_hz: 60,
      ..Controls::default()
    }
  }

  /// Walks a player back and forth, which is the input pattern that produces
  /// the most cell boundaries per second and therefore the most chances to
  /// disagree.
  fn patrol(world: &mut World, seat: usize, secs: u64, controls: &Controls) {
    let dirs = [Dir::Right, Dir::Down, Dir::Left, Dir::Up];
    for i in 0..(secs * 4) {
      world.input(seat, Intent::Walk(dirs[(i % 4) as usize]), controls);
      world.run(250, controls);
    }
  }

  #[test]
  fn a_perfect_link_produces_no_snaps() {
    // The control. Anything above zero here is a rule written twice, not a
    // network effect, and it would invalidate every other measurement.
    let c = quiet();
    let mut world = World::new(&c, B0MB_SEED);
    patrol(&mut world, 0, 8, &c);
    assert_eq!(world.total_snaps(), 0, "no latency, no loss, one shared rule: nothing to correct");
    assert_eq!(world.disagreement(0), Some(0));
  }

  #[test]
  fn latency_alone_still_produces_no_snaps() {
    // The measurement that says prediction is worth having. A round trip of
    // delay is not by itself a disagreement: the client is ahead, not wrong,
    // and the history comparison is what keeps that from reading as an error.
    //
    // The playout depth has to cover the link, which is the condition the next
    // test is about; here it does.
    let c = Controls {
      latency_ms: 120,
      jitter_ms: 30,
      playout_delay_ms: 250,
      ..quiet()
    };
    let mut world = World::new(&c, B0MB_SEED);
    patrol(&mut world, 0, 8, &c);
    assert_eq!(world.total_snaps(), 0, "delay is not disagreement");
  }

  #[test]
  fn a_link_slower_than_the_playout_depth_has_its_inputs_refused_and_snaps() {
    // The condition an arena has to admit players against, and the reason the
    // horde example measures a connection before seating it. An input is named
    // for `press + playout`, so a one-way delay longer than the playout depth
    // (plus the late window) lands after the tick it named and is dropped.
    //
    // The client predicted it anyway, so the two sides ran different inputs,
    // and on a lattice that can only be resolved by jumping. It is the same
    // failure as packet loss and it is produced here by a link that is merely
    // slow, which is why it deserves its own test.
    let c = Controls {
      latency_ms: 400,
      jitter_ms: 0,
      playout_delay_ms: 100,
      input_max_late_ticks: 4,
      ..quiet()
    };
    let mut world = World::new(&c, B0MB_SEED);
    patrol(&mut world, 0, 10, &c);

    let (_, _, closed, _, margin) = world.server.input_verdicts()[0];
    assert!(closed > 0, "the server refused inputs that arrived past their tick");
    assert!(margin.is_some_and(|m| m < 0), "and refused them on the late side: {margin:?}");
    assert!(world.total_snaps() > 0, "which the client can only resolve by snapping");
  }

  #[test]
  fn losing_inputs_is_what_actually_snaps_a_player() {
    // And this is the case prediction cannot save: an input the server never
    // saw means the two sides ran different inputs, which on a lattice can only
    // be resolved by jumping.
    let c = Controls {
      latency_ms: 80,
      loss_pct: 45.0,
      ..quiet()
    };
    let mut world = World::new(&c, B0MB_SEED);
    patrol(&mut world, 0, 12, &c);
    assert!(world.total_snaps() > 0, "dropped inputs put the two sides in different cells");
    // However much it snapped, it must end up agreeing: a correction that does
    // not converge is a bug, not a trade-off.
    world.run(3_000, &c);
    assert_eq!(world.disagreement(0), Some(0), "and it always converges back");
  }

  #[test]
  fn a_snap_is_always_a_whole_number_of_cells() {
    // The property that makes this example what it is. There is no such thing
    // as a fractional correction here, so the mean snap distance is an integer
    // count of cells and never a smoothed number.
    let c = Controls {
      latency_ms: 90,
      loss_pct: 40.0,
      ..quiet()
    };
    let mut world = World::new(&c, B0MB_SEED);
    patrol(&mut world, 0, 12, &c);
    if world.total_snaps() > 0 {
      assert!(world.clients[0].snapped_cells >= world.clients[0].snaps, "every snap moved at least one whole cell");
    }
  }

  #[test]
  fn the_playout_buffer_decides_a_contested_cell_by_press_time_not_by_ping() {
    // The fairness claim, and the reason inputs are tick-addressed. Two players
    // press at the same instant with very different links; the buffer is what
    // makes the outcome the same either way.
    //
    // Measured as: does the near player's advantage change when the far player
    // is made much slower? With playout on, both inputs execute on the tick
    // they named, so the answer is no.
    let run_with = |playout: bool, far_latency: u64| {
      let c = Controls {
        latency_ms: 20,
        input_playout: playout,
        playout_delay_ms: 200,
        input_max_late_ticks: 20,
        input_max_early_ticks: 40,
        ..quiet()
      };
      let mut world = World::new(&c, B0MB_SEED);
      // Both walk right at the same simulated instant. The far player's input
      // is delayed on the wire; its named tick is not.
      world.input(0, Intent::Walk(Dir::Right), &c);
      let far = Controls {
        latency_ms: far_latency,
        ..c
      };
      world.input(1, Intent::Walk(Dir::Right), &far);
      world.run(1_200, &c);
      (world.server.players[0].cell, world.server.players[1].cell)
    };

    let (_, fast_far) = run_with(true, 20);
    let (_, slow_far) = run_with(true, 220);
    assert_eq!(fast_far, slow_far, "with playout, a slow link reaches the same cell as a fast one");
  }

  #[test]
  fn a_chain_reaction_reaches_every_client_as_one_event() {
    // A cascade split across two messages would let a client draw the first
    // arm before it knows the second bomb exists, which is a flash in the wrong
    // place on the one frame anybody is looking at it.
    let c = quiet();
    let mut world = World::new(&c, B0MB_SEED);
    for x in 1..6u8 {
      world.server.grid.set(Cell::new(x, 1), Tile::Empty);
    }
    world.server.players[0].blast_radius = 3;
    world.server.players[1].cell = Cell::new(3, 1);
    world.server.players[1].blast_radius = 3;

    world.input(0, Intent::Bomb, &c);
    world.run(400, &c);
    world.input(1, Intent::Bomb, &c);
    world.run(FUSE_MS + 400, &c);

    assert!(world.server.longest_chain >= 2, "the two bombs chained: {}", world.server.longest_chain);
  }

  #[test]
  fn a_round_ends_and_every_client_is_given_the_new_board() {
    let c = quiet();
    let mut world = World::new(&c, B0MB_SEED);
    let first = world.clients[0].grid.clone();
    world.server.players[1].alive = false;
    world.run(ROUND_END_MS + 500, &c);

    assert_eq!(world.server.round(), 2);
    assert!(world.clients.iter().all(|client| client.rounds_seen >= 2), "everyone was handed the new round");
    assert_ne!(world.clients[0].grid, first, "and it is a different board");
  }

  #[test]
  fn a_client_never_holds_a_wall_the_server_has_destroyed() {
    // The stale-board case: a client predicting against a wall that is gone
    // refuses a step the server allows, and manufactures a snap out of nothing.
    let c = quiet();
    let mut world = World::new(&c, B0MB_SEED);
    world.input(0, Intent::Bomb, &c);
    world.run(FUSE_MS + 800, &c);

    for y in 0..GRID_H {
      for x in 0..GRID_W {
        let cell = Cell::new(x, y);
        assert_eq!(
          world.clients[0].grid.get(cell),
          world.server.grid.get(cell),
          "client and server disagree about {cell:?}"
        );
      }
    }
  }

  #[test]
  fn holding_a_direction_through_the_round_interval_does_not_snap() {
    // The server stops simulating once a round is settled, so the last
    // explosion stays readable. Nothing in a frame says so: the players simply
    // stop moving, which looks exactly like everybody standing still.
    //
    // A client that keeps predicting through it walks a player the server is
    // deliberately freezing, and *every frame of the interval* is a correction
    // invented out of a rule the client was never told. Holding a direction is
    // what makes it obvious, and holding one is what a player does when they
    // have just won and are still leaning on the key.
    let c = quiet();
    let mut world = World::new(&c, B0MB_SEED);
    world.run(500, &c);
    let before = world.total_snaps();

    // Settle the round, then keep walking into it.
    world.server.players[1].alive = false;
    world.input(0, Intent::Walk(Dir::Right), &c);
    world.run(ROUND_END_MS - 200, &c);

    assert!(world.server.round_over_pending(), "the interval is actually running");
    assert_eq!(world.total_snaps(), before, "a paused server is not predicted through");
  }

  #[test]
  fn the_next_round_resumes_prediction() {
    // And the pause has to lift, or the player is frozen on a live board.
    let c = quiet();
    let mut world = World::new(&c, B0MB_SEED);
    world.server.players[1].alive = false;
    world.run(ROUND_END_MS + 400, &c);
    assert!(!world.clients[0].is_paused(), "a new board is the server simulating again");

    let start = world.clients[0].my_player().cell;
    world.input(0, Intent::Walk(Dir::Right), &c);
    world.run(800, &c);
    assert_ne!(world.clients[0].my_player().cell, start, "and the prediction runs again");
  }

  /// Holds one direction across open ground, which is the case a player
  /// describes as "running around freely": the most cell boundaries per second
  /// and therefore the most chances for the two sides to cross one at different
  /// moments.
  fn sprint(world: &mut World, seat: usize, secs: u64, controls: &Controls) {
    for y in 1..GRID_H - 1 {
      for x in 1..GRID_W - 1 {
        world.server.grid.set(Cell::new(x, y), Tile::Empty);
      }
    }
    for client in &mut world.clients {
      for y in 1..GRID_H - 1 {
        for x in 1..GRID_W - 1 {
          client.grid.set(Cell::new(x, y), Tile::Empty);
        }
      }
    }
    // Back and forth along a clear row, turning only at the ends, so nearly all
    // the time is spent crossing boundaries rather than starting and stopping.
    for i in 0..(secs * 2) {
      let dir = if i % 2 == 0 { Dir::Right } else { Dir::Left };
      world.input(seat, Intent::Walk(dir), controls);
      world.run(1_500, controls);
    }
  }

  #[test]
  fn crossing_open_ground_on_a_steady_link_does_not_snap() {
    // The case the tick-driven prediction exists for. A player crossing cell
    // after cell gives the two sides a boundary to disagree about several times
    // a second, and a client stepping on its own frame grid rather than the
    // server's crosses every one of them at a different moment.
    let c = Controls {
      latency_ms: 40,
      jitter_ms: 0,
      playout_delay_ms: 100,
      ..quiet()
    };
    let mut world = World::new(&c, B0MB_SEED);
    sprint(&mut world, 0, 12, &c);
    assert_eq!(world.total_snaps(), 0, "a steady link crossing open ground has nothing to correct");
  }

  #[test]
  fn jitter_past_the_playout_depth_is_what_is_left() {
    // What remains once the prediction runs on the server's own tick grid, and
    // it is a real network effect rather than a bug: jitter beyond the playout
    // depth pushes an input past the tick it named, the schedule runs it late
    // or refuses it, and the client had predicted it on time. That is a genuine
    // disagreement, and on a lattice a genuine disagreement is a snap.
    //
    // Paired with the test above on purpose. A snap count means nothing unless
    // the case that should produce none actually produces none.
    let c = Controls {
      latency_ms: 40,
      jitter_ms: 220,
      playout_delay_ms: 60,
      input_max_late_ticks: 2,
      ..quiet()
    };
    let mut world = World::new(&c, B0MB_SEED);
    sprint(&mut world, 0, 12, &c);

    let (_, late, closed, _, _) = world.server.input_verdicts()[0];
    assert!(late + closed > 0, "jitter put inputs past the tick they named");
    assert!(world.total_snaps() > 0, "and the client had predicted them on time");
  }
}
