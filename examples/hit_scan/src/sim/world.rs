//! Server and clients in one process, with an impaired link between them.
//!
//! The harness every claim in this example is measured on. It is not the
//! networked build: it stands in for the wire so a rule can be tested without a
//! socket, and so a measurement can be repeated exactly rather than played.
//!
//! One thing it deliberately cannot measure: every party here reads the same
//! clock, where the networked build has to estimate it. Clock error is
//! [`crate::net::client`]'s problem and is tested there.

use plaza_client_utils::net_sim::{LatencyLink, Rng};

use crate::sim::client::Client;
use crate::sim::protocol::{Op, ServerPolicy};
use crate::sim::server::Server;
use crate::sim::types::{Controls, Dir8, PlayerId, SIM_STEP_MS, V2, Weapon};

const IMPAIR_SEED: u64 = 0x5CA1_AB1E;

/// One process holding the authority and every client, with a link between.
pub struct World {
  pub server: Server,
  pub clients: Vec<Client>,
  down: Vec<LatencyLink<Op>>,
  up: LatencyLink<(usize, Op)>,
  rng: Rng,
  /// Per client, whichever of the two directions is being impaired. One-sided
  /// impairment is the falsifier for anything that claims to be fair: if a
  /// number moves when only *your* sending is delayed, the rule is reading
  /// arrival order somewhere.
  pub one_way_only: bool,
}

impl World {
  pub fn new(controls: &Controls, seed: u64) -> Self {
    let count = controls.players.clamp(1, crate::sim::types::MAX_SEATS);
    let mut server = Server::new(count, seed);
    let mut clients = Vec::new();
    for seat in 0..count {
      server.take_seat(seat);
      let mut client = Client::new(seat as PlayerId, controls.render_delay_ms);
      client.on_op(
        Op::Welcome {
          player: seat as PlayerId,
          policy: policy_of(controls, count),
          start: Box::new(server.start()),
        },
        0,
      );
      clients.push(client);
    }
    Self {
      server,
      clients,
      down: (0..count).map(|_| LatencyLink::default()).collect(),
      up: LatencyLink::default(),
      rng: Rng::new(IMPAIR_SEED),
      one_way_only: false,
    }
  }

  pub fn now_ms(&self) -> u64 {
    self.server.now_ms()
  }

  fn send_up(&mut self, seat: usize, op: Op, controls: &Controls) {
    let now = self.server.now_ms();
    self.up.send(now, (seat, op), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut self.rng);
  }

  /// Holds a direction, the way a real client does: through its own predictor
  /// first, then onto the wire.
  pub fn walk(&mut self, seat: usize, dir: Dir8, controls: &Controls) {
    let now = self.server.now_ms();
    let Some(client) = self.clients.get_mut(seat) else { return };
    if let Some(op) = client.press(dir, now) {
      self.send_up(seat, op, controls);
    }
  }

  pub fn shoot(&mut self, seat: usize, aim: V2, weapon: Weapon, controls: &Controls) {
    let now = self.server.now_ms();
    let Some(client) = self.clients.get_mut(seat) else { return };
    if let Some(op) = client.shoot(aim, weapon, now) {
      self.send_up(seat, op, controls);
    }
  }

  /// One simulation step for everybody.
  pub fn step(&mut self, dt_ms: u64, controls: &Controls) {
    let now = self.server.now_ms();
    let arrived = self.up.drain_due(now);
    for (seat, op) in arrived {
      match op {
        Op::Move { tick, dir, .. } => {
          self.server.submit(seat, tick, crate::sim::protocol::Intent::Walk(dir), controls);
        }
        Op::Shoot { tick, aim_deg, weapon, .. } => {
          self.server.submit(seat, tick, crate::sim::protocol::Intent::Shoot { aim_deg, weapon }, controls);
        }
        _ => {}
      }
    }

    let out = self.server.advance(dt_ms, controls);
    let now = self.server.now_ms();

    let mut outbound: Vec<Op> = Vec::new();
    for shot in out.shots {
      outbound.push(Op::Shot(Box::new(shot)));
    }
    for death in out.deaths {
      outbound.push(Op::Died(Box::new(death)));
    }
    // Frames last: a client should learn the cause before it sees the world
    // that already contains the effect.
    for frame in out.frames {
      outbound.push(Op::Frame(Box::new(frame)));
    }

    for (seat, link) in self.down.iter_mut().enumerate() {
      // The downstream half is left clean when only one direction is being
      // impaired, so a test can delay a player's *sending* alone.
      let (latency, jitter, loss) = if self.one_way_only && seat == 0 {
        (0, 0, 0.0)
      } else {
        (controls.latency_ms, controls.jitter_ms, controls.loss_pct)
      };
      for op in &outbound {
        link.send(now, op.clone(), latency, jitter, loss, &mut self.rng);
      }
    }

    for seat in 0..self.clients.len() {
      let due = self.down[seat].drain_due(now);
      for op in due {
        self.clients[seat].on_op(op, now);
      }
      self.clients[seat].advance(dt_ms, now, controls);
    }
  }

  /// Runs for a stretch of game time in whole quanta.
  pub fn run(&mut self, ms: u64, controls: &Controls) {
    let steps = ms / SIM_STEP_MS;
    for _ in 0..steps {
      self.step(SIM_STEP_MS, controls);
    }
  }

  pub fn total_snaps(&self) -> u64 {
    self.clients.iter().map(|c| c.stats.snaps).sum()
  }

  /// Mean distance between where clients draw somebody and where the server
  /// has them **now**.
  ///
  /// The figure this repository has quoted everywhere, and it is not honest: a
  /// client drawing a render delay behind is charged the whole of that delay as
  /// if it were error. Kept so the panel can show it next to the honest one,
  /// because the gap between the two is the point.
  pub fn mean_render_error_naive(&self, controls: &Controls) -> f32 {
    let truth = self.server.snaps_now();
    let mut sum = 0.0;
    let mut n = 0u32;
    for client in &self.clients {
      for (id, drawn, _) in client.render(controls) {
        if id == client.me {
          continue;
        }
        if let Some((_, snap)) = truth.iter().find(|(tid, _)| *tid == id) {
          sum += drawn.dist(snap.pos);
          n += 1;
        }
      }
    }
    if n == 0 { 0.0 } else { sum / n as f32 }
  }

  /// Mean distance between where clients draw somebody and where the server had
  /// them **at the instant that client is drawing**.
  ///
  /// The honest figure. It needs a truth history, which is exactly what the
  /// server already keeps to rewind a shot, so measuring it correctly costs a
  /// second call to a buffer that has to exist anyway.
  pub fn mean_render_error_honest(&self, controls: &Controls) -> f32 {
    let mut sum = 0.0;
    let mut n = 0u32;
    for client in &self.clients {
      let Some(at) = client.render_at() else { continue };
      let truth = self.server.snaps_at(at);
      for (id, drawn, _) in client.render(controls) {
        if id == client.me {
          continue;
        }
        if let Some((_, snap)) = truth.iter().find(|(tid, _)| *tid == id) {
          sum += drawn.dist(snap.pos);
          n += 1;
        }
      }
    }
    if n == 0 { 0.0 } else { sum / n as f32 }
  }

}

pub fn policy_of(controls: &Controls, players: usize) -> ServerPolicy {
  ServerPolicy {
    sync_hz: controls.sync_hz,
    playout_delay_ms: controls.playout_delay_ms,
    render_delay_ms: controls.render_delay_ms,
    input_max_late_ticks: controls.input_max_late_ticks,
    input_max_early_ticks: controls.input_max_early_ticks,
    rewind: controls.rewind,
    rewind_budget_ms: controls.rewind_budget_ms(),
    allow_ghost: controls.allow_ghost,
    players,
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::sim::types::Rewind;

  const SEED: u64 = 0x4D_1A_9C_02;

  fn base() -> Controls {
    Controls {
      bots: false,
      players: 4,
      latency_ms: 0,
      jitter_ms: 0,
      loss_pct: 0.0,
      predict_self: true,
      interpolate_peers: true,
      extrapolate_peers: false,
      ..Controls::default()
    }
  }

  /// Everybody walking a different pattern, so the peers a client draws are
  /// actually moving. Every render-error number is zero in a still world, which
  /// is a very convincing way to measure nothing.
  fn patrol(world: &mut World, controls: &Controls, ms: u64) {
    const DIRS: [Dir8; 6] = [Dir8::E, Dir8::S, Dir8::W, Dir8::N, Dir8::Se, Dir8::Nw];
    let mut t = 0;
    while t < ms {
      for seat in 0..world.clients.len() {
        let dir = DIRS[((t / 400) as usize + seat) % DIRS.len()];
        world.walk(seat, dir, controls);
      }
      world.run(200, controls);
      t += 200;
    }
  }

  /// Patrolling, and everybody shooting at whoever they can see.
  ///
  /// Aim is taken from what the *client* is drawing, never from server truth: a
  /// harness that aims at the real position is a harness in which lag
  /// compensation has nothing to compensate.
  fn skirmish(world: &mut World, controls: &Controls, ms: u64) {
    const DIRS: [Dir8; 6] = [Dir8::E, Dir8::S, Dir8::W, Dir8::N, Dir8::Se, Dir8::Nw];
    let mut t = 0;
    while t < ms {
      for seat in 0..world.clients.len() {
        let dir = DIRS[((t / 500) as usize + seat * 2) % DIRS.len()];
        world.walk(seat, dir, controls);

        let drawn = world.clients[seat].render(controls);
        let me = world.clients[seat].me;
        let Some(from) = drawn.iter().find(|(id, _, alive)| *id == me && *alive).map(|(_, p, _)| *p) else { continue };
        let target = drawn
          .iter()
          .filter(|(id, _, alive)| *id != me && *alive)
          .filter(|(_, p, _)| crate::sim::rules::line_of_sight(from, *p))
          .min_by(|a, b| from.dist(a.1).total_cmp(&from.dist(b.1)));
        if let Some((_, at, _)) = target {
          let aim = at.sub(from);
          world.shoot(seat, aim, Weapon::Rifle, controls);
        }
      }
      world.run(100, controls);
      t += 100;
    }
  }

  #[test]
  fn latency_alone_produces_no_disagreement() {
    // The claim every playout scheme rests on: delay is not disagreement.
    // Prediction removes the round trip and nothing else, and the input runs on
    // the tick it named on both sides, so a slow link is behind rather than
    // wrong.
    let controls = Controls { latency_ms: 120, jitter_ms: 30, playout_delay_ms: 200, ..base() };
    let mut world = World::new(&controls, SEED);
    patrol(&mut world, &controls, 8000);
    assert_eq!(world.total_snaps(), 0, "a delayed client was corrected");
  }

  #[test]
  fn a_link_slower_than_the_input_window_is_corrected_constantly() {
    // The other side of it, and the reason the window is a setting. Past
    // `playable_one_way_ms` every input names a tick that has already closed,
    // so the player's own movement is refused and their prediction is fiction.
    let controls = Controls { latency_ms: 600, playout_delay_ms: 100, input_max_late_ticks: 4, ..base() };
    assert!(controls.latency_ms > controls.playable_one_way_ms(), "the premise");
    let mut world = World::new(&controls, SEED);
    patrol(&mut world, &controls, 6000);
    assert!(world.total_snaps() > 0, "a link this slow cannot be merely behind");
  }

  #[test]
  fn the_honest_render_error_is_smaller_than_the_one_this_repository_quotes() {
    // `mean_render_error` compares a drawn position against server truth *now*,
    // so it charges a client for a render delay it is taking deliberately. The
    // honest figure compares against truth at the instant being drawn, and the
    // gap between the two is roughly the delay times the speed.
    let controls = Controls { render_delay_ms: 150, sync_hz: 20, ..base() };
    let mut world = World::new(&controls, SEED);
    patrol(&mut world, &controls, 6000);

    let naive = world.mean_render_error_naive(&controls);
    let honest = world.mean_render_error_honest(&controls);
    assert!(naive > honest, "naive {naive:.1} honest {honest:.1}");
    assert!(honest < naive * 0.6, "the correction should be large, not marginal: naive {naive:.1} honest {honest:.1}");
  }

  #[test]
  fn raising_the_render_delay_inflates_the_naive_error_and_leaves_the_honest_one_alone() {
    // The sharpest form of it. A deeper buffer is a *choice*, and a metric that
    // punishes it is measuring the choice rather than the netcode. Only one of
    // these two numbers is allowed to move.
    let shallow = Controls { render_delay_ms: 50, sync_hz: 20, ..base() };
    let deep = Controls { render_delay_ms: 250, sync_hz: 20, ..base() };

    let mut a = World::new(&shallow, SEED);
    patrol(&mut a, &shallow, 6000);
    let mut b = World::new(&deep, SEED);
    patrol(&mut b, &deep, 6000);

    let naive_a = a.mean_render_error_naive(&shallow);
    let naive_b = b.mean_render_error_naive(&deep);
    let honest_a = a.mean_render_error_honest(&shallow);
    let honest_b = b.mean_render_error_honest(&deep);

    assert!(naive_b > naive_a * 1.5, "the naive figure tracks the delay: {naive_a:.1} -> {naive_b:.1}");
    assert!(
      (honest_b - honest_a).abs() < honest_a.max(honest_b) * 0.75 + 2.0,
      "the honest one should barely move: {honest_a:.1} -> {honest_b:.1}"
    );
  }

  #[test]
  fn delaying_only_one_players_sending_does_not_change_where_anybody_ends_up() {
    // The falsifier. If any rule in here reads arrival order rather than the
    // tick a client named, impairing one direction alone will move a result.
    let controls = Controls { latency_ms: 150, playout_delay_ms: 250, ..base() };

    let mut fair = World::new(&controls, SEED);
    patrol(&mut fair, &controls, 5000);

    let mut skewed = World::new(&controls, SEED);
    skewed.one_way_only = true;
    patrol(&mut skewed, &controls, 5000);

    for seat in 0..fair.server.players.len() {
      let a = fair.server.players[seat].pos;
      let b = skewed.server.players[seat].pos;
      assert!(a.dist(b) < 0.5, "seat {seat} ended at {a:?} against {b:?}");
    }
  }

  #[test]
  fn a_rewound_server_charges_targets_for_their_killers_latency_and_an_unrewound_one_does_not() {
    // Both halves of the trade in one comparison, over the same script. This is
    // the number the panel exists to show: turning the rewind on does not make
    // the game fairer, it moves who is being treated unfairly.
    // Every seat here is a connected client, so `bots` would steer nobody: the
    // harness has to pull the triggers itself, which is also the honest thing
    // to measure, since it is the client-to-wire-to-rewind path that is on
    // trial rather than the bot.
    let with = Controls { rewind: Rewind::Uncapped, latency_ms: 200, playout_delay_ms: 250, ..base() };
    let without = Controls { rewind: Rewind::Off, ..with };

    let mut a = World::new(&with, SEED);
    skirmish(&mut a, &with, 12000);
    let mut b = World::new(&without, SEED);
    skirmish(&mut b, &without, 12000);

    assert!(a.server.stats.shots_fired > 0 && b.server.stats.shots_fired > 0, "both had a fight");
    assert_eq!(b.server.stats.granted_by_rewind, 0, "nothing can be granted by a rewind that is off");
    assert!(
      b.server.stats.deaths_behind_cover <= a.server.stats.deaths_behind_cover,
      "refusing to look back cannot produce more deaths behind cover: {} against {}",
      b.server.stats.deaths_behind_cover,
      a.server.stats.deaths_behind_cover
    );
  }

  #[test]
  fn a_client_runs_ahead_of_the_newest_frame_it_has_seen() {
    // At or below zero the client is naming input ticks the server has already
    // run, and every one of them is refused. It is the single number that says
    // whether the whole scheme is working.
    let controls = Controls { latency_ms: 100, playout_delay_ms: 150, ..base() };
    let mut world = World::new(&controls, SEED);
    patrol(&mut world, &controls, 4000);
    for client in &world.clients {
      assert!(client.lead_ticks() > 0, "seat {} leads by {}", client.me, client.lead_ticks());
    }
  }
}
