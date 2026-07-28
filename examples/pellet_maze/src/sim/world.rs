//! Server and clients in one process, with an impaired link between them.
//!
//! The harness every claim in this example is measured on. As in `bomb_grid`,
//! the one thing it structurally cannot model is a client *estimating* the
//! clock: here the two halves share one, which is exactly what a real client
//! does not have.

use plaza_client_utils::net_sim::{LatencyLink, Rng};

use crate::sim::client::Client;
use crate::sim::protocol::Op;
use crate::sim::server::{Audience, Server};
use crate::sim::types::*;

const IMPAIR_SEED: u64 = 0x5EED_1A2E;

pub struct World {
  pub server: Server,
  pub clients: Vec<Client>,
  /// What each seat was actually handed on the last step, so a test can assert
  /// about what crossed the wire rather than about what the state ended up as.
  /// Secrecy is a property of what is *sent*, and it is only testable here.
  pub delivered: Vec<Vec<Op>>,
  down: Vec<LatencyLink<Op>>,
  up: LatencyLink<(usize, u64, Dir)>,
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
      client.set_turn_buffer(controls.turn_buffer_ms);
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

  /// Asks for a turn, aimed the way a real client aims one.
  pub fn turn(&mut self, seat: usize, dir: Dir, controls: &Controls) {
    self.seq += 1;
    let now = self.server.now_ms();
    let tick = (now + controls.playout_delay_ms) / SIM_STEP_MS;
    if let Some(client) = self.clients.get_mut(seat) {
      client.schedule_turn(self.seq, tick, dir);
    }
    self.up.send(now, (seat, tick, dir), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut self.rng);
  }

  pub fn step(&mut self, dt_ms: u64, controls: &Controls) {
    let now = self.server.now_ms();
    for (seat, tick, dir) in self.up.drain_due(now) {
      self.server.submit(seat, tick, dir, controls);
    }

    let out = self.server.advance(dt_ms, controls);
    let now = self.server.now_ms();

    let mut outbound: Vec<Op> = Vec::new();
    if let Some(round) = out.round_start {
      outbound.push(Op::Round(Box::new(round)));
    }
    if let Some((runner, by, next_in_ms)) = out.caught {
      outbound.push(Op::Caught { runner, by, next_in_ms });
    }
    // Two lists: see the arena. A turn report is only read by the player it
    // names, and an event about an invisible player must reach nobody else.
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

    for (seat, link) in self.down.iter_mut().enumerate() {
      for op in &outbound {
        link.send(now, op.clone(), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut self.rng);
      }
      for (_, op) in private.iter().filter(|(id, _)| *id as usize == seat) {
        link.send(now, op.clone(), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut self.rng);
      }
      // The frame is **this seat's**, not everybody's: a hidden runner is
      // absent from the others' copies.
      if let Some((_, frame)) = out.frames.iter().find(|(id, _)| *id as usize == seat) {
        link.send(now, Op::Frame(Box::new(frame.clone())), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut self.rng);
      }
    }

    for row in self.delivered.iter_mut() {
      row.clear();
    }

    for (seat, client) in self.clients.iter_mut().enumerate() {
      // Drained **before** the tick, which is the order a real client runs in:
      // `poll` then `tick`. Ticking first means a client acts on a whole tick
      // of stale knowledge, and the one that matters is being told the world
      // has stopped: predicting one tick past a freeze is a correction the
      // client did not have to make.
      for op in self.down[seat].drain_due(now) {
        self.delivered[seat].push(op.clone());
        match op {
          Op::Frame(frame) => client.on_frame(&frame, controls),
          Op::Round(round) => client.on_round(&round),
          Op::TurnTaken(taken) => client.on_turn_taken(&taken),
          Op::Eaten { cells, .. } => client.on_eaten(&cells),
          Op::PowerTaken { cell, .. } => client.on_power_taken(cell),
          Op::Caught { .. } => client.set_paused(true),
          _ => {}
        }
      }
      client.tick(now, controls);
    }
  }

  /// Runs past the opening countdown, for a test that wants to measure play
  /// rather than the pause before it.
  pub fn start_playing(&mut self, controls: &Controls) {
    while self.server.countdown_ms().is_some() {
      self.step(SIM_STEP_MS, controls);
    }
  }

  pub fn run(&mut self, ms: u64, controls: &Controls) {
    for _ in 0..(ms / SIM_STEP_MS) {
      self.step(SIM_STEP_MS, controls);
    }
  }

  pub fn total_snaps(&self) -> u64 {
    self.clients.iter().map(|c| c.snaps).sum()
  }

  pub fn total_wrong_junctions(&self) -> u64 {
    self.clients.iter().map(|c| c.wrong_junction).sum()
  }

  pub fn disagreement(&self, seat: usize) -> Option<u16> {
    let client = self.clients.get(seat)?;
    let truth = self.server.players.get(seat)?;
    Some(client.my_player().cell.distance(truth.cell))
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

  #[test]
  fn a_turn_report_goes_only_to_the_player_it_names() {
    // Not secrecy machinery: a client discards every `TurnTaken` that is not
    // its own, so broadcasting them put every player's exact junction on
    // everybody's wire for no reader at all.
    let c = quiet();
    let mut world = World::new(&c, MAZE_SEED);
    world.start_playing(&c);
    let mut seen = 0;
    for i in 0..600 {
      if i % 8 == 0 {
        for seat in 0..world.clients.len() {
          let at = world.server.players[seat].cell;
          let exits = world.server.maze.exits(at);
          if let Some(dir) = exits.get(i as usize % exits.len().max(1)).copied() {
            world.turn(seat, dir, &c);
          }
        }
      }
      world.run(SIM_STEP_MS, &c);
      for (seat, ops) in world.delivered.iter().enumerate() {
        for op in ops {
          if let Op::TurnTaken(taken) = op {
            seen += 1;
            assert_eq!(taken.player as usize, seat, "seat {seat} was told where somebody else turned");
          }
        }
      }
    }
    assert!(seen > 0, "the test is worthless unless turns were reported at all");
  }

  #[test]
  fn nothing_on_the_wire_says_where_a_hidden_player_is() {
    // The end to end version of the property, taken from the wire rather than
    // from the server's intent: every op a seat is handed, checked against the
    // hidden player's actual cell.
    let c = Controls { bots: true, ..quiet() };
    let mut world = World::new(&c, MAZE_SEED);
    world.start_playing(&c);
    let runner = world.server.runner_seat();
    world.server.players[runner].hidden_until_ms = world.server.now_ms() + VANISH_MS;

    let mut checked = 0;
    while world.server.players[runner].hidden(world.server.now_ms()) {
      let at = world.server.players[runner].occupied();
      world.run(SIM_STEP_MS, &c);
      // The tick the vanish expires is not part of the property: everything
      // held back goes out on it, and by then the player is in the frames
      // again, so the cells it names are no longer secret.
      if !world.server.players[runner].hidden(world.server.now_ms()) {
        break;
      }
      for (seat, ops) in world.delivered.iter().enumerate() {
        if seat == runner {
          continue;
        }
        for op in ops {
          checked += 1;
          match op {
            Op::Frame(frame) => assert!(
              !frame.players.iter().any(|p| p.id as usize == runner),
              "a frame carried the hidden player"
            ),
            Op::Eaten { cells, .. } => assert!(!cells.contains(&at), "an eaten pellet gave the cell away"),
            Op::PowerTaken { cell, .. } => assert!(*cell != at, "a taken power-up gave the cell away"),
            Op::TurnTaken(taken) => assert!(taken.at != at, "a turn report gave the junction away"),
            _ => {}
          }
        }
      }
    }
    assert!(checked > 0, "some ops were actually delivered");
  }

  /// Turns at random from the exits actually available, which is what a player
  /// running a maze does and what gives the turn queue the most to resolve.
  fn wander(world: &mut World, seat: usize, secs: u64, controls: &Controls) {
    world.start_playing(controls);
    for i in 0..(secs * 4) {
      let at = world.server.players[seat].cell;
      let exits = world.server.maze.exits(at);
      if exits.is_empty() {
        world.run(250, controls);
        continue;
      }
      let dir = exits[(i as usize * 7 + seat) % exits.len()];
      world.turn(seat, dir, controls);
      world.run(250, controls);
    }
  }

  #[test]
  fn a_perfect_link_never_turns_at_the_wrong_junction() {
    // The control, and the one that matters most here: a wrong junction on a
    // perfect link would mean the turn rule is written twice, and every other
    // measurement in this file would be meaningless.
    let c = quiet();
    let mut world = World::new(&c, MAZE_SEED);
    wander(&mut world, 0, 10, &c);
    assert_eq!(world.total_wrong_junctions(), 0, "same maze, same rule, same junctions");
    assert_eq!(world.total_snaps(), 0, "and nothing to correct");
  }

  #[test]
  fn latency_alone_still_turns_at_the_right_junction() {
    // The measurement that says predicting a place-triggered input is worth
    // doing at all. A client running a round trip ahead reaches each junction
    // before the server does, and still has to reach the *same* one.
    let c = Controls {
      latency_ms: 120,
      jitter_ms: 30,
      playout_delay_ms: 250,
      ..quiet()
    };
    let mut world = World::new(&c, MAZE_SEED);
    wander(&mut world, 0, 10, &c);
    assert_eq!(world.total_wrong_junctions(), 0, "being early is not being wrong");
    assert_eq!(world.total_snaps(), 0);
  }

  #[test]
  fn losing_a_turn_request_is_what_sends_the_two_sides_down_different_corridors() {
    // The failure this example exists to show, and the reason it is counted
    // apart from a cell correction: the client took a turn the server never
    // heard about, so the two are not one cell apart, they are in different
    // parts of the maze, and the gap grows until a frame settles it.
    let c = Controls {
      latency_ms: 60,
      loss_pct: 55.0,
      ..quiet()
    };
    let mut world = World::new(&c, MAZE_SEED);
    wander(&mut world, 0, 14, &c);
    assert!(
      world.total_snaps() > 0,
      "dropped turn requests put the two sides in different corridors"
    );
    // However far apart they got, it must converge: a correction that does not
    // settle is a bug rather than a trade-off.
    world.run(4_000, &c);
    assert_eq!(world.disagreement(0), Some(0), "and it always converges back");
  }

  #[test]
  fn a_turn_that_expires_expires_on_both_sides() {
    // The buffer is a *server* setting for exactly this reason. A client with a
    // longer buffer than the server would take a turn the server had already
    // forgotten, and then run down a corridor the server never entered: a
    // wrong junction manufactured out of a mismatched constant rather than out
    // of the network.
    let c = Controls {
      turn_buffer_ms: 120,
      ..quiet()
    };
    let mut world = World::new(&c, MAZE_SEED);
    world.start_playing(&c);

    // Aim at a wall repeatedly, which is the way to make turns expire.
    for _ in 0..12 {
      let at = world.server.players[0].cell;
      let blocked = Dir::ALL.into_iter().find(|d| at.step(*d).is_none_or(|c| !world.server.maze.open(c)));
      if let Some(blocked) = blocked {
        world.turn(0, blocked, &c);
      }
      world.run(400, &c);
    }
    let (_, server_expired) = world.server.turn_stats()[0];
    let (_, client_expired) = world.clients[0].turn_stats();
    assert!(server_expired > 0, "the test needs turns to have expired: {server_expired}");
    assert_eq!(world.total_wrong_junctions(), 0, "and both sides forgot the same ones");
    let _ = client_expired;
  }

  #[test]
  fn a_round_ends_when_the_runner_is_caught_and_every_client_is_told() {
    let c = quiet();
    let mut world = World::new(&c, MAZE_SEED);
    world.start_playing(&c);
    let at = world.server.players[0].occupied();
    world.server.players[1].cell = at;
    world.server.players[1].step = None;
    world.run(ROUND_END_MS + 600, &c);

    assert_eq!(world.server.round(), 2);
    assert!(world.clients.iter().all(|c| c.rounds_seen >= 2), "everyone was handed the new round");
    assert!(!world.clients[0].is_paused(), "and the pause lifted with it");
  }

  #[test]
  fn a_paused_round_is_not_predicted_through() {
    // The server holds everyone still after a catch. A client that keeps
    // predicting walks a player the server is deliberately freezing, and here
    // that is worse than in `bomb_grid`: a runner never stops, so the
    // prediction would cross several cells of the interval.
    let c = quiet();
    let mut world = World::new(&c, MAZE_SEED);
    world.start_playing(&c);
    world.run(400, &c);
    let before = world.total_snaps();

    let at = world.server.players[0].occupied();
    world.server.players[1].cell = at;
    world.server.players[1].step = None;
    world.run(ROUND_END_MS - 400, &c);

    assert!(world.server.round_over_pending(), "the interval is running");
    assert_eq!(world.total_snaps(), before, "a paused server is not predicted through");
  }

  #[test]
  fn the_runner_eats_and_every_client_sees_the_pellet_go() {
    let c = quiet();
    let mut world = World::new(&c, MAZE_SEED);
    world.start_playing(&c);
    let before = world.clients[0].pellets.len();
    world.run(2_000, &c);
    assert!(world.server.pellets_eaten > 0, "the runner ate something");
    assert!(world.clients[0].pellets.len() < before, "and the client saw it go");
  }
}
