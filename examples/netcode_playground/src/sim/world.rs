//! One `step` that advances the whole picture: sample input, push it up the
//! wire, let the server tick, bring packets back down, fold them in.
//!
//! Everything here is host-native and headless, so the tests at the bottom are
//! where the `client_utils` behaviour is actually pinned. The renderer only ever
//! reads the results of `step`.

use plaza_client_utils::FixedTimestep;
use plaza_client_utils::net_sim::{LatencyLink, Rng};

use crate::sim::client::Client;
use crate::sim::server::ToyServer;
use plaza_wire::payloads::{Ping, Pong};

use crate::sim::types::{BoxState, ClientMsg, Controls, EntityId, MoveInput, ServerMsg, Shot, Vec2, ARENA_H, ARENA_W, PING_INTERVAL_FRAMES, STEP_MS};

/// A fired shot, kept briefly so the renderer can show the aim point and, once
/// the verdict returns down the wire, whether it hit.
#[derive(Clone, Copy, Debug)]
pub struct RecentShot {
  pub aim: Vec2,
  /// `None` until the server's verdict arrives; then `Some(hit)`.
  pub hit: Option<Option<EntityId>>,
  pub age_secs: f32,
}

pub struct World {
  server: ToyServer,
  client: Client,
  up: LatencyLink<ClientMsg>,
  down: LatencyLink<ServerMsg>,
  rng: Rng,

  wall_ms: u64,
  /// When the client samples its input. A fixed rate independent of the frame
  /// rate, so what is sent does not depend on how fast the browser is drawing.
  input_step: FixedTimestep,
  recent_shot: Option<RecentShot>,
  last_ping_ms: u64,
}

impl World {
  pub fn new(bot_count: u8, seed: u64) -> Self {
    let start = BoxState {
      pos: Vec2::new(ARENA_W * 0.5, ARENA_H * 0.5),
      vel: Vec2::default(),
    };
    Self {
      server: ToyServer::new(bot_count),
      client: Client::new(start),
      up: LatencyLink::new(),
      down: LatencyLink::new(),
      rng: Rng::new(seed),
      wall_ms: 0,
      input_step: FixedTimestep::from_step_ms(STEP_MS),
      recent_shot: None,
      last_ping_ms: 0,
    }
  }

  fn send_up(&mut self, msg: ClientMsg, c: &Controls) {
    self.up.send(self.wall_ms, msg, c.latency_ms, c.jitter_ms, c.loss_pct, &mut self.rng);
  }

  fn send_down(&mut self, msg: ServerMsg, c: &Controls) {
    self.down.send(self.wall_ms, msg, c.latency_ms, c.jitter_ms, c.loss_pct, &mut self.rng);
  }

  /// Fires a shot at `aim` (a field coordinate), stamped with the server time the
  /// client is currently seeing. Sent up the wire like any other message.
  pub fn shoot(&mut self, aim: Vec2, controls: &Controls) {
    let shot = Shot {
      aim,
      aim_time: self.client.view_time(),
    };
    self.recent_shot = Some(RecentShot {
      aim,
      hit: None,
      age_secs: 0.0,
    });
    self.send_up(ClientMsg::Shot(shot), controls);
  }

  /// The client's measured round trip to the server.
  pub fn client_rtt_ms(&self) -> Option<f32> {
    self.client.rtt_ms()
  }

  /// The server's measured round trip to the player.
  pub fn server_rtt_ms(&self) -> Option<f32> {
    self.server.rtt_ms()
  }

  /// The client's measured jitter.
  pub fn jitter_ms(&self) -> Option<f32> {
    self.client.jitter_ms()
  }

  /// The interpolation delay currently in effect on the client.
  pub fn interp_delay_ms(&self) -> u64 {
    self.client.interp_delay_ms()
  }

  /// The render clock's current playback rate (1.0 is real time).
  pub fn clock_playback_rate(&self) -> f32 {
    self.client.clock_playback_rate()
  }

  /// Advances by `dt_ms`, holding `input` for any input steps that fall in it.
  pub fn step(&mut self, dt_ms: u64, input: MoveInput, controls: &Controls) {
    self.wall_ms += dt_ms;

    for _ in self.input_step.advance(dt_ms) {
      let cmd = self.client.sample_input(input, controls);
      self.send_up(ClientMsg::Cmd(cmd), controls);
    }

    // Each side pings the other periodically to measure its round trip. Both are
    // stamped with the shared wall clock, so each measures the real link delay.
    if self.wall_ms - self.last_ping_ms >= PING_INTERVAL_FRAMES * STEP_MS {
      self.last_ping_ms = self.wall_ms;
      let ping = Ping { origin_time_ms: self.wall_ms };
      self.send_up(ClientMsg::Ping(ping), controls);
      self.send_down(ServerMsg::Ping(ping), controls);
    }

    // Deliver whatever has arrived at the server. Inputs queue for the next tick;
    // shots resolve immediately; pings are echoed; pongs measure the round trip.
    for msg in self.up.drain_due(self.wall_ms) {
      match msg {
        ClientMsg::Cmd(cmd) => self.server.receive(cmd),
        ClientMsg::Shot(shot) => {
          let result = self.server.resolve_shot(shot, controls.lag_comp);
          self.send_down(ServerMsg::ShotResult(result), controls);
        }
        ClientMsg::Ping(p) => self.send_down(
          ServerMsg::Pong(Pong { origin_time_ms: p.origin_time_ms, responder_time_ms: self.wall_ms }),
          controls,
        ),
        ClientMsg::Pong(p) => self.server.observe_rtt(self.wall_ms.saturating_sub(p.origin_time_ms)),
      }
    }
    for packet in self.server.advance(dt_ms, controls.server_step_ms()) {
      self.send_down(ServerMsg::State(packet), controls);
    }

    for msg in self.down.drain_due(self.wall_ms) {
      match msg {
        ServerMsg::State(packet) => self.client.on_packet(packet, self.wall_ms, controls),
        ServerMsg::ShotResult(result) => {
          if let Some(shot) = self.recent_shot.as_mut() {
            shot.hit = Some(result.hit);
          }
        }
        ServerMsg::Ping(p) => self.send_up(
          ClientMsg::Pong(Pong { origin_time_ms: p.origin_time_ms, responder_time_ms: self.wall_ms }),
          controls,
        ),
        ServerMsg::Pong(p) => self.client.observe_rtt(self.wall_ms.saturating_sub(p.origin_time_ms)),
      }
    }
    self.client.tick(dt_ms);

    if let Some(shot) = self.recent_shot.as_mut() {
      shot.age_secs += dt_ms as f32 / 1000.0;
      if shot.age_secs > 1.2 {
        self.recent_shot = None;
      }
    }
  }

  /// The shot to draw, if one is still fresh.
  pub fn recent_shot(&self) -> Option<RecentShot> {
    self.recent_shot
  }

  pub fn you_render(&self, c: &Controls) -> BoxState {
    self.client.you_render(c)
  }

  /// The exact predicted state, independent of smoothing: what the tests assert
  /// on, so they check the prediction logic rather than how it is drawn.
  pub fn you_logical(&self) -> BoxState {
    self.client.you_logical()
  }

  pub fn you_ghost(&self) -> BoxState {
    self.client.you_ghost()
  }

  pub fn remotes_render(&self, c: &Controls) -> Vec<(EntityId, BoxState)> {
    self.client.remotes_render(c)
  }

  /// The server's real positions, drawn faintly so the interpolation lag on the
  /// remotes is visible against the truth.
  pub fn truth(&self) -> Vec<(EntityId, BoxState)> {
    self.server.truth()
  }

  /// Whether a given remote is being dead reckoned this frame.
  pub fn extrapolating(&self, id: EntityId) -> bool {
    self.client.extrapolating(id)
  }

  pub fn prediction_error(&self) -> f32 {
    self.client.prediction_error()
  }

  pub fn unacked_inputs(&self) -> usize {
    self.client.unacked_inputs()
  }

  pub fn latest_seq(&self) -> u64 {
    self.client.latest_seq()
  }

  pub fn acked_seq(&self) -> u64 {
    self.client.acked_seq()
  }

  pub fn packets_in_flight(&self) -> usize {
    self.up.in_flight() + self.down.in_flight()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn right() -> MoveInput {
    MoveInput { dx: 1.0, dy: 0.0 }
  }

  fn idle() -> MoveInput {
    MoveInput { dx: 0.0, dy: 0.0 }
  }

  fn run(world: &mut World, frames: usize, input: MoveInput, c: &Controls) {
    for _ in 0..frames {
      world.step(16, input, c);
    }
  }

  /// A deterministic sweep of directions, so a stress run keeps the box moving,
  /// hitting walls, and reversing, without any randomness in the test itself.
  fn wander(frame: usize) -> MoveInput {
    let dirs = [(1.0, 0.0), (0.0, 1.0), (-1.0, 0.0), (0.0, -1.0), (1.0, 1.0), (-1.0, -1.0)];
    let (dx, dy) = dirs[(frame / 7) % dirs.len()];
    MoveInput { dx, dy }
  }

  /// Mean distance between where the client draws each remote and where the
  /// server truly has it, accumulated over a run.
  fn mean_remote_error(c: &Controls, frames: usize, seed: u64) -> f32 {
    let mut world = World::new(3, seed);
    let (mut sum, mut n) = (0.0f32, 0u32);
    for frame in 0..frames {
      world.step(16, wander(frame), c);
      let truth: Vec<(EntityId, BoxState)> = world.truth();
      for (id, drawn) in world.remotes_render(c) {
        // Only the frames actually being dead reckoned. Every other frame is an
        // interpolation, identical under both policies, and averaging them in
        // buries the difference under a constant both sides pay.
        if !world.extrapolating(id) {
          continue;
        }
        if let Some((_, actual)) = truth.iter().find(|(t, _)| *t == id) {
          sum += drawn.pos.dist(actual.pos);
          n += 1;
        }
      }
    }
    if n == 0 { 0.0 } else { sum / n as f32 }
  }

  #[test]
  fn a_fitted_curve_only_pays_once_the_gaps_are_long() {
    // The measured shape of the technique, and it is narrower than it sounds.
    //
    // The acceleration term goes as dt squared, so over a short gap it is worth
    // thousandths of a pixel. At the demo's normal server rate the render target
    // never gets more than a few milliseconds past the newest snapshot (the
    // adaptive buffer sees to that), and second order changes nothing at all. It
    // starts to pay only when snapshots are hundreds of milliseconds apart.
    let slow = Controls {
      latency_ms: 120,
      jitter_ms: 30,
      loss_pct: 15.0,
      server_hz: 5,
      extrapolate: true,
      adaptive_buffer: false,
      ..Controls::default()
    };
    let first = mean_remote_error(&Controls { second_order: false, ..slow }, 900, 0xC0FFEE);
    let second = mean_remote_error(&Controls { second_order: true, ..slow }, 900, 0xC0FFEE);
    // 2%, and it used to measure 7%. The difference was not the curve, which is
    // unchanged at 17.79 px: it was the *tangent* improving from 19.12 to 18.15
    // when `ExtrapolationBase` stopped rewinding to the raw sample past its cap
    // and started holding at the cap instead. Most of this technique's apparent
    // advantage was an artifact of a discontinuity in what it was compared
    // against. Recorded here because a comparative measurement is only ever as
    // good as its baseline, and a bug in the baseline flatters the challenger.
    assert!(second < first * 0.99, "at 5 Hz the curve should still beat the tangent: {second:.2}px against {first:.2}px");

    // And at a normal rate it is simply inert, which is the part worth pinning:
    // it means the toggle cannot be sold as a general improvement.
    let normal = Controls { server_hz: 30, ..slow };
    let n_first = mean_remote_error(&Controls { second_order: false, ..normal }, 900, 0xC0FFEE);
    let n_second = mean_remote_error(&Controls { second_order: true, ..normal }, 900, 0xC0FFEE);
    // Absolute rather than relative, because at 30 Hz the buffer does not starve
    // at all and both figures are zero. A ratio test cannot express "there was
    // nothing here to improve", which is exactly the result.
    assert!(
      (n_second - n_first).abs() < 0.5,
      "at 30 Hz it should change essentially nothing: {n_second:.2}px against {n_first:.2}px"
    );
  }

  #[test]
  fn the_fit_does_not_hurt_when_the_stream_is_healthy() {
    // The check that matters more than the win. A technique that helps under loss
    // and costs accuracy the rest of the time is not worth switching on, because
    // the rest of the time is most of the time: with packets arriving, the render
    // target sits inside the buffer and interpolation handles it, so the curve
    // should almost never be consulted.
    let healthy = Controls {
      latency_ms: 40,
      jitter_ms: 0,
      loss_pct: 0.0,
      extrapolate: true,
      ..Controls::default()
    };
    let first = mean_remote_error(&Controls { second_order: false, ..healthy }, 600, 0xC0FFEE);
    let second = mean_remote_error(&Controls { second_order: true, ..healthy }, 600, 0xC0FFEE);
    assert!(second <= first * 1.05, "no regression on a clean link: {second:.2}px against {first:.2}px");
  }

  #[test]
  fn survives_a_hostile_network_with_every_invariant_intact() {
    // High latency, heavy jitter (reorders packets), and a third of packets
    // dropped, both directions, for ten seconds of simulation.
    let c = Controls {
      latency_ms: 220,
      jitter_ms: 180,
      loss_pct: 33.0,
      ..Controls::default()
    };
    let mut w = World::new(3, 0xBADC0FFEE);

    let mut prev_seq = 0u64;
    let mut fired = false;
    for frame in 0..600 {
      w.step(16, wander(frame), &c);

      // Shoot occasionally, exercising the lag-comp path under loss too.
      if frame % 90 == 45 {
        w.shoot(Vec2::new(400.0, 300.0), &c);
        fired = true;
      }

      // Sequence numbers only ever increase.
      assert!(w.latest_seq() >= prev_seq, "sequence went backwards at frame {frame}");
      prev_seq = w.latest_seq();

      // The predicted box stays a real, finite position: no NaN, no explosion.
      let me = w.you_render(&c).pos;
      assert!(me.x.is_finite() && me.y.is_finite(), "predicted position not finite at frame {frame}");
      assert!(w.prediction_error().is_finite(), "prediction error not finite at frame {frame}");

      // Remotes render to finite positions too.
      for (id, s) in w.remotes_render(&c) {
        assert!(s.pos.x.is_finite() && s.pos.y.is_finite(), "remote {id} not finite at frame {frame}");
      }
    }

    assert!(fired, "the stress run fired at least one shot");
    // Even under a third loss, the acknowledged sequence made real progress.
    assert!(w.acked_seq() > 0, "the server acknowledged some inputs");
    assert!(w.acked_seq() <= w.latest_seq(), "cannot acknowledge more than was sent");
  }

  #[test]
  fn prediction_converges_after_reconciliation() {
    let c = Controls {
      latency_ms: 0,
      ..Controls::default()
    };
    let mut w = World::new(1, 42);
    let start_x = w.you_render(&c).pos.x;

    run(&mut w, 40, right(), &c);
    assert!(w.you_render(&c).pos.x > start_x, "the predicted box moved right");

    // Once input stops and packets drain, prediction and authority agree.
    run(&mut w, 120, idle(), &c);
    assert!(w.prediction_error() < 2.0, "error was {}", w.prediction_error());

    // The client sends a command every frame, so a short tail of just-sent ones
    // is always awaiting the next server tick; everything older is acknowledged.
    assert!(w.latest_seq() - w.acked_seq() <= 8, "unacked tail bounded, was {}", w.latest_seq() - w.acked_seq());
  }

  #[test]
  fn inputs_stay_unacknowledged_while_in_flight() {
    let c = Controls {
      latency_ms: 300,
      ..Controls::default()
    };
    let mut w = World::new(1, 7);
    run(&mut w, 20, right(), &c);

    // At 300ms one-way latency, recent inputs cannot have been acknowledged yet,
    // so reconciliation has something to replay.
    assert!(w.unacked_inputs() > 0, "expected inputs still in flight");
    assert!(w.latest_seq() > w.acked_seq());
  }

  #[test]
  fn reconciliation_off_lets_the_prediction_drift_through_a_wall() {
    let c = Controls {
      latency_ms: 60,
      predict: true,
      reconcile: false,
      ..Controls::default()
    };
    let mut w = World::new(1, 3);

    // Hold right long past the wall. The server clamps its box inside the arena;
    // the client, never reconciled, keeps predicting straight through it.
    run(&mut w, 300, right(), &c);

    let predicted = w.you_logical().pos.x;
    let authoritative = w.you_ghost().pos.x;
    assert!(authoritative <= ARENA_W - 16.0 + 0.5, "server kept its box in the arena: {authoritative}");
    assert!(predicted > authoritative + 50.0, "prediction drifted past the wall: pred {predicted} vs auth {authoritative}");
  }

  #[test]
  fn reconciliation_on_holds_the_prediction_at_the_wall() {
    let c = Controls {
      latency_ms: 60,
      predict: true,
      reconcile: true,
      ..Controls::default()
    };
    let mut w = World::new(1, 3);
    run(&mut w, 300, right(), &c);

    // With reconciliation on, the same wall push stays *bounded* near the clamp:
    // the prediction overshoots by about one round trip of travel and is pulled
    // back every packet, rather than running away as the reconcile-off case does.
    // (Contrast this cap with the unbounded drift asserted in the test above.)
    let clamp_x = ARENA_W - 16.0;
    let overshoot = w.you_logical().pos.x - clamp_x;
    assert!(overshoot >= 0.0, "prediction is at least at the wall");
    assert!(overshoot < 80.0, "overshoot stays about one round trip, was {overshoot}");
  }

  #[test]
  fn lag_compensation_lands_a_shot_the_present_would_miss() {
    // Fire at where the client currently *renders* a moving bot (its past,
    // interpolated position), then let the verdict return. This drives the whole
    // chain: the server records history and rewinds through
    // `plaza_server_utils::HistoricalStateBuffer`.
    fn fire(lag_comp: bool) -> Option<Option<u8>> {
      let c = Controls {
        latency_ms: 160,
        lag_comp,
        ..Controls::default()
      };
      let mut w = World::new(2, 55);
      run(&mut w, 240, idle(), &c); // establish history and interpolation
      let (_, seen) = w.remotes_render(&c)[0];
      w.shoot(seen.pos, &c);
      run(&mut w, 50, idle(), &c); // longer than the round trip
      w.recent_shot().and_then(|s| s.hit)
    }

    assert!(matches!(fire(true), Some(Some(_))), "with lag comp, the shot hits the rewound target");
    assert_eq!(fire(false), Some(None), "without it, the same shot misses: the bot has moved on");
  }

  #[test]
  fn adaptive_buffering_grows_the_delay_under_jitter() {
    let jittery = Controls {
      latency_ms: 100,
      jitter_ms: 150,
      adaptive_buffer: true,
      ..Controls::default()
    };
    let mut w = World::new(1, 11);
    run(&mut w, 300, idle(), &jittery);

    assert!(w.jitter_ms().unwrap() > 10.0, "the estimator saw real jitter");
    assert!(w.interp_delay_ms() > 150, "the buffer grew past the base under jitter, got {}", w.interp_delay_ms());

    // The same connection with a fixed buffer ignores the jitter.
    let fixed = Controls {
      adaptive_buffer: false,
      ..jittery
    };
    let mut w2 = World::new(1, 11);
    run(&mut w2, 300, idle(), &fixed);
    assert_eq!(w2.interp_delay_ms(), 150, "fixed delay does not react to jitter");
  }

  #[test]
  fn a_low_server_rate_is_handled_by_the_client() {
    let slow = Controls {
      server_hz: 5, // 200ms between snapshots
      latency_ms: 80,
      ..Controls::default()
    };
    let mut w = World::new(2, 22);
    run(&mut w, 400, idle(), &slow);

    // The client interpolates the coarse stream to finite positions...
    for (id, s) in w.remotes_render(&slow) {
      assert!(s.pos.x.is_finite() && s.pos.y.is_finite(), "remote {id} finite at a low server rate");
    }
    // ...and the adaptive buffer sized itself to the coarse rate (>= 1.5 steps).
    assert!(w.interp_delay_ms() >= 300, "delay covers the 200ms step, got {}", w.interp_delay_ms());
  }

  #[test]
  fn round_trip_latency_is_measured_on_both_sides() {
    let c = Controls {
      latency_ms: 100,
      jitter_ms: 0,
      loss_pct: 0.0,
      ..Controls::default()
    };
    let mut w = World::new(1, 5);
    run(&mut w, 200, idle(), &c); // several ping round trips complete

    let client_rtt = w.client_rtt_ms().expect("client measured its RTT");
    let server_rtt = w.server_rtt_ms().expect("server measured its RTT");

    // Round trip is one hop each way (~200ms), plus up to a frame of delivery
    // quantisation on each leg.
    for (who, rtt) in [("client", client_rtt), ("server", server_rtt)] {
      assert!((190.0..270.0).contains(&rtt), "{who} RTT should be near 200ms, got {rtt}");
    }
  }

  #[test]
  fn the_smooth_clock_dilates_its_rate_to_absorb_a_latency_jump() {
    // Smooth clock on: when latency jumps, the render clock should adjust its
    // playback *rate* (glide) rather than snap its position, then settle back to
    // real time, all while keeping remotes at finite positions.
    let base = Controls {
      latency_ms: 60,
      smooth_clock: true,
      ..Controls::default()
    };
    let mut w = World::new(2, 88);
    run(&mut w, 200, idle(), &base); // settle at a steady rate near 1.0
    assert!((w.clock_playback_rate() - 1.0).abs() < 0.02, "steady stream runs near real time, got {}", w.clock_playback_rate());

    // Latency jumps: the estimate now drifts from the stream, so the rate should
    // leave 1.0 to correct, but stay within the model's +/-10% bound.
    let jumped = Controls { latency_ms: 260, ..base };
    let mut moved_off_real_time = false;
    for _ in 0..60 {
      w.step(16, idle(), &jumped);
      let rate = w.clock_playback_rate();
      assert!((0.89..=1.11).contains(&rate), "rate stays within the model bound, got {rate}");
      if (rate - 1.0).abs() > 0.01 {
        moved_off_real_time = true;
      }
    }
    assert!(moved_off_real_time, "the clock dilated its rate to absorb the jump");

    // It converges back toward real time as the estimate realigns.
    run(&mut w, 400, idle(), &jumped);
    assert!((w.clock_playback_rate() - 1.0).abs() < 0.05, "rate settles back near real time, got {}", w.clock_playback_rate());
    for (id, s) in w.remotes_render(&jumped) {
      assert!(s.pos.x.is_finite() && s.pos.y.is_finite(), "remote {id} finite throughout");
    }
  }

  #[test]
  fn dead_reckoning_keeps_a_remote_moving_when_packets_stop() {
    let mut w = World::new(1, 7);
    run(&mut w, 120, idle(), &Controls::default()); // establish interpolation

    // Cut the network: 100% loss stops new snapshots. The render clock keeps
    // advancing, so the interpolation target runs past the newest snapshot.
    let cut = Controls {
      loss_pct: 100.0,
      ..Controls::default()
    };
    run(&mut w, 30, idle(), &cut);

    let with = w.remotes_render(&Controls { extrapolate: true, ..cut })[0].1;
    let without = w.remotes_render(&Controls { extrapolate: false, ..cut })[0].1;

    // Without extrapolation the buffer clamps to the frozen newest snapshot; with
    // it, the bot is dead-reckoned forward along its velocity.
    assert!(with.pos.dist(without.pos) > 1.0, "dead reckoning should move the remote past the frozen snapshot");
  }

  #[test]
  fn interpolation_places_remotes_between_snapshots() {
    let c = Controls::default();
    let mut w = World::new(2, 99);
    run(&mut w, 200, idle(), &c);

    let on = w.remotes_render(&Controls { interpolate: true, ..c });
    let off = w.remotes_render(&Controls { interpolate: false, ..c });
    assert_eq!(on.len(), 2, "two bots reported");

    // The bots are always moving, so rendering a moment in the past (interpolated)
    // cannot land on the same point as the newest raw snapshot.
    let moved: f32 = on.iter().zip(&off).map(|((_, a), (_, b))| a.pos.dist(b.pos)).sum();
    assert!(moved > 0.5, "interpolation should differ from the raw newest snapshot, sum {moved}");
  }
}
