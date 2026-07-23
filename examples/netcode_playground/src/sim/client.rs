//! The client, built from `plaza_client_utils`.
//!
//! It uses the drop-in bundles: [`PredictedPlayer`] for the local box (predict,
//! reconcile, smooth) and one [`RemoteView`] per remote (interpolate, extrapolate,
//! hold). The teaching toggles are honoured by rendering the right piece rather
//! than by disabling the bundles: the ghost and the raw-vs-smoothed choice live
//! here, in the demo.

use std::collections::BTreeMap;

use plaza_client_utils::interpolation::InterpolationClock;
use plaza_client_utils::types::SequenceNumber;
use plaza_client_utils::trajectory::TrajectoryPredictor;
use plaza_client_utils::{smoothstep, PlayerConfig, PredictedPlayer, RemoteView, RenderOpts, RttEstimator};

use crate::sim::types::{
  apply_input, BoxState, ClientCmd, Controls, EntityId, MoveInput, ServerPacket, Vec2, BASE_DELAY_STEPS, EXTRAP_MAX_MS, INTERP_DELAY_MS, JITTER_FACTOR, MAX_DELAY_MS, PLAYBACK_RATE_ADJUST, SYNC_STRENGTH,
};

const INPUT_BUFFER: usize = 256;
/// Enough snapshots to cover the largest interpolation delay even at the highest
/// server rate (600ms of history at 60 Hz).
const SNAPSHOT_BUFFER: usize = 48;
const SMOOTH_SECS: f32 = 0.12;

/// Blends two boxes, for correction smoothing.
fn lerp_box(a: &BoxState, b: &BoxState, t: f32) -> BoxState {
  BoxState {
    pos: Vec2::new(a.pos.x + (b.pos.x - a.pos.x) * t, a.pos.y + (b.pos.y - a.pos.y) * t),
    vel: b.vel,
  }
}

pub struct Client {
  me: PredictedPlayer<BoxState, MoveInput>,

  /// The last authoritative local box, tracked here so the ghost and the
  /// prediction-off view keep updating even when reconciliation is toggled off.
  auth_you: BoxState,

  remotes: BTreeMap<EntityId, RemoteView<BoxState, Vec2>>,
  /// A second-order fit per remote, one predictor per axis, fed the same
  /// snapshots the view gets.
  ///
  /// Kept beside `RemoteView` rather than inside it: the view is generic over the
  /// application's state type, and a curve fit needs arithmetic on the value
  /// itself. Rather than force a vector-space bound on every consumer of the
  /// drop-in type, the app runs the scalar primitive on the axes it cares about,
  /// which is two lines here.
  curves: BTreeMap<EntityId, (TrajectoryPredictor, TrajectoryPredictor)>,
  clock: InterpolationClock<u64>,
  /// Whether the clock is being rate-synced (glide) rather than position-synced
  /// or free-running, so [`tick`](Self::tick) knows to dilate the advance.
  rate_synced: bool,

  /// The client's measured round trip to the server.
  rtt: RttEstimator,
}

impl Client {
  pub fn new(initial: BoxState) -> Self {
    let config = PlayerConfig {
      input_buffer: INPUT_BUFFER,
      smoothing_secs: SMOOTH_SECS,
      // A smoothstep ease starts and stops the correction gently, so the
      // rubber-band on a misprediction looks like a slide rather than a ramp.
      easing: smoothstep,
    };
    Self {
      me: PredictedPlayer::new(initial, config, apply_input, lerp_box),
      auth_you: initial,
      remotes: BTreeMap::new(),
      curves: BTreeMap::new(),
      clock: InterpolationClock::new(INTERP_DELAY_MS),
      rate_synced: false,
      rtt: RttEstimator::default(),
    }
  }

  /// Records a measured round trip to the server (from a returned ping).
  pub fn observe_rtt(&mut self, sample_ms: u64) {
    self.rtt.observe(sample_ms);
  }

  /// The client's smoothed round trip to the server, if measured yet.
  pub fn rtt_ms(&self) -> Option<f32> {
    self.rtt.rtt_ms()
  }

  /// The measured jitter, if any.
  pub fn jitter_ms(&self) -> Option<f32> {
    self.rtt.jitter_ms()
  }

  /// The interpolation delay currently in effect (fixed or adaptive).
  pub fn interp_delay_ms(&self) -> u64 {
    self.clock.delay()
  }

  /// Samples one input step. Always predicts and sends: prediction is an internal
  /// detail the renderer shows or hides, and the server needs the input either way.
  pub fn sample_input(&mut self, input: MoveInput, _controls: &Controls) -> ClientCmd {
    let seq = self.me.input(input);
    ClientCmd { seq, input }
  }

  pub fn on_packet(&mut self, packet: ServerPacket, controls: &Controls) {
    let (auth_you, acked_seq) = packet.you;
    self.auth_you = auth_you;

    // Reconcile only when both prediction and reconciliation are on; otherwise
    // the prediction is left to drift, which the toggle is there to show.
    if controls.predict && controls.reconcile {
      self.me.reconcile(auth_you, acked_seq);
    }

    // Size the interpolation delay from the server rate (need at least a step or
    // two of history) plus the measured jitter, or hold a fixed delay. This is
    // the buffering *policy*; plaza supplies the jitter and the settable delay.
    // Set it before syncing: the rate model scales its drift by this delay.
    let delay = if controls.adaptive_buffer {
      let base = BASE_DELAY_STEPS * controls.server_step_ms() as f32;
      let jitter = self.rtt.jitter_ms().unwrap_or(0.0);
      ((base + JITTER_FACTOR * jitter) as u64).min(MAX_DELAY_MS)
    } else {
      INTERP_DELAY_MS
    };
    self.clock.set_delay(delay);

    // Keep the render target aligned to the snapshot stream, so it self-corrects
    // as latency drifts instead of running off. Two ways to align: nudge the
    // clock's position each packet (resync, a small snap), or dilate its playback
    // rate so it glides in (smooth clock). Free-run (init once) if sync is off.
    self.rate_synced = false;
    if controls.clock_sync {
      if controls.smooth_clock {
        self.clock.observe_rate(packet.server_time_ms, PLAYBACK_RATE_ADJUST);
        self.rate_synced = true;
      } else {
        self.clock.resync(packet.server_time_ms, SYNC_STRENGTH);
      }
    } else {
      self.clock.observe(packet.server_time_ms);
    }

    for (id, state) in packet.remotes {
      self
        .remotes
        .entry(id)
        .or_insert_with(|| RemoteView::new(SNAPSHOT_BUFFER, EXTRAP_MAX_MS))
        .push(packet.server_time_ms, state, state.vel);
      let curve = self
        .curves
        .entry(id)
        .or_insert_with(|| (TrajectoryPredictor::new(1.0, EXTRAP_MAX_MS), TrajectoryPredictor::new(1.0, EXTRAP_MAX_MS)));
      curve.0.observe(packet.server_time_ms, state.pos.x);
      curve.1.observe(packet.server_time_ms, state.pos.y);
    }
  }

  pub fn tick(&mut self, dt_ms: u64) {
    // Rate-synced playback dilates the advance; otherwise it is real time.
    if self.rate_synced {
      self.clock.advance_scaled(dt_ms);
    } else {
      self.clock.advance(dt_ms);
    }
    self.me.advance(dt_ms as f32 / 1000.0);
  }

  /// The render clock's current playback rate (1.0 is real time). Deviates from
  /// 1.0 while smooth-clock sync is gliding the target into alignment.
  pub fn clock_playback_rate(&self) -> f32 {
    self.clock.playback_rate()
  }

  pub fn view_time(&self) -> u64 {
    self.clock.target().unwrap_or(0)
  }

  /// Where to draw the local box: the authoritative state with prediction off,
  /// the smoothed prediction with smoothing on, the raw prediction otherwise.
  pub fn you_render(&self, controls: &Controls) -> BoxState {
    if !controls.predict {
      return self.auth_you;
    }
    if controls.smooth {
      self.me.render()
    } else {
      *self.me.logical()
    }
  }

  pub fn you_logical(&self) -> BoxState {
    *self.me.logical()
  }

  pub fn you_ghost(&self) -> BoxState {
    self.auth_you
  }

  pub fn remotes_render(&self, controls: &Controls) -> Vec<(EntityId, BoxState)> {
    let target = self.clock.target();
    let opts = RenderOpts {
      interpolate: controls.interpolate,
      extrapolate: controls.extrapolate,
    };
    self
      .remotes
      .iter()
      .filter_map(|(id, view)| {
        let mut state = view.render(target, opts)?;
        // Second order only applies where first order was going to be used at
        // all: past the newest snapshot, with extrapolation enabled. Inside the
        // buffer, interpolation between two known states beats any fit.
        if controls.extrapolate
          && controls.second_order
          && let (Some(t), Some((cx, cy))) = (target, self.curves.get(id))
          && cx.newest_time().is_some_and(|newest| t > newest)
        {
              // The predictors are held at full trust and the damping is applied
              // here, by blending against the first-order answer the view already
              // produced. Same result as damping inside the fit, and it keeps the
              // coefficient a runtime control rather than a rebuild.
          let d = controls.curve_damping.clamp(0.0, 1.0);
          if let (Some(x), Some(y)) = (cx.predict(t), cy.predict(t)) {
            state.pos = Vec2::new(state.pos.x + (x - state.pos.x) * d, state.pos.y + (y - state.pos.y) * d);
          }
        }
        Some((*id, state))
      })
      .collect()
  }

  /// Whether this remote is currently being *dead reckoned* rather than
  /// interpolated: the render target has passed its newest snapshot.
  ///
  /// Exposed because without it there is no way to measure an extrapolation
  /// change. Averaged over every frame, the error is dominated by the
  /// interpolation delay that both policies share, and a real difference in the
  /// extrapolated frames disappears into it.
  pub fn extrapolating(&self, id: EntityId) -> bool {
    match (self.clock.target(), self.curves.get(&id)) {
      (Some(target), Some((cx, _))) => cx.newest_time().is_some_and(|newest| target > newest),
      _ => false,
    }
  }

  pub fn prediction_error(&self) -> f32 {
    self.me.logical().pos.dist(self.auth_you.pos)
  }

  pub fn unacked_inputs(&self) -> usize {
    self.me.unacked_count()
  }

  pub fn latest_seq(&self) -> SequenceNumber {
    self.me.latest_seq()
  }

  pub fn acked_seq(&self) -> SequenceNumber {
    self.me.acked_seq()
  }
}
