//! Prediction for a server that holds an input and integrates it, rather than
//! consuming one input per step.
//!
//! # Which of the two models is yours
//!
//! There are two ways a server can consume client input, and they need different
//! reconciliation. Choosing wrong is silent: the prediction is simply always a
//! little bit off, in a way that looks like network jitter.
//!
//! | model | what the server does | use |
//! |---|---|---|
//! | discrete | each input advances the simulation exactly one step | [`crate::PredictedPlayer`] |
//! | continuous | an input sets a *held* value the server integrates every tick | this |
//!
//! Input replay is wrong for the continuous model, and wrong in a way that gets
//! worse the more you economise on bandwidth. The client replays one input as
//! one step while the server applied that direction for however many ticks
//! passed, so the replay under-counts and the prediction sits permanently
//! behind. Coalescing input (sending only on change, plus a keepalive) makes it
//! dramatically worse, because one input can cover a second of simulation.
//!
//! The continuous model is what a game arrives at whenever input is coalesced or
//! sent below the simulation rate, which is most twitch games with any bandwidth
//! pressure at all.
//!
//! # How the correction works, and why there is no separate render state
//!
//! [`crate::PredictedPlayer`] keeps an exact logical state and eases only what is
//! drawn, because replaying inputs over an authoritative state reproduces an
//! exact answer worth keeping. Here there is nothing to replay and so no exact
//! answer: the client is dead reckoning, and the honest thing is to bend the
//! prediction itself toward the server a little at a time. So `logical` and
//! `render` are the same value, and [`blend`](HeldInputConfig::blend) is the
//! ease.
//!
//! **The correction is continuous on purpose.** Letting error accumulate until
//! it crosses a threshold and then closing the whole gap at once produces a
//! metronomic drift-snap-drift-snap that a player feels as a rhythmic tug, even
//! at zero latency. Bending a fraction of the gap every packet absorbs the same
//! drift invisibly. A hard snap is for a genuine discontinuity only, which is
//! what [`with_teleport`](HeldInputPredictor::with_teleport) is for: the choice
//! between easing and snapping is made by *cause*, never by magnitude.

use std::fmt::Debug;

use crate::correction::Correction;

/// How a [`HeldInputPredictor`] corrects itself.
#[derive(Debug, Clone, Copy)]
pub struct HeldInputConfig {
  /// The fraction of the remaining gap to the server closed on each
  /// [`reconcile`](HeldInputPredictor::reconcile), between 0 and 1.
  ///
  /// Higher converges faster and follows the server more tightly; lower stays
  /// smoother and leads more on local input. Zero disables correction entirely,
  /// which is pure dead reckoning and will drift without bound.
  pub blend: f32,
}

impl Default for HeldInputConfig {
  fn default() -> Self {
    Self { blend: 0.25 }
  }
}

/// A locally dead-reckoned entity whose server holds its input and integrates
/// it. See the [module docs](self) for when this is the right primitive.
///
/// ```ignore
/// let mut me = HeldInputPredictor::new(start, HeldInputConfig::default(), integrate, lerp)
///   .with_teleport(distance, 200.0);
///
/// // Every step: hold what the player is asking for and dead reckon it.
/// me.hold(direction);
/// me.advance(dt_secs);
///
/// // On an authoritative packet, whose state is one one-way delay old:
/// let correction = me.reconcile(packet.position, one_way_secs);
/// monitor.record(distance(&correction.seen, &correction.settled));
/// ```
pub struct HeldInputPredictor<State: Clone + Debug, Input: Clone + Debug, Ctx = ()> {
  state: State,
  held: Input,
  advance: fn(&mut State, &Input, f32, &Ctx),
  lerp: fn(&State, &State, f32) -> State,
  blend: f32,
  ctx: Ctx,
  active: bool,
  teleport: Option<(fn(&State, &State) -> f32, f32)>,
}

impl<State: Clone + Debug, Input: Clone + Debug + Default, Ctx: Default> HeldInputPredictor<State, Input, Ctx> {
  /// `advance` must be the **server's** integration rule, not a client
  /// approximation of it: `(state, held_input, dt_secs, context)`. Anything the
  /// server does that this leaves out arrives as a permanent correction, and
  /// tracking that down later is far more expensive than sharing the function
  /// now. `Ctx` is the world a forced entity reads its forces from, and `()` for
  /// an entity moved only by its own input.
  pub fn new(
    initial: State,
    config: HeldInputConfig,
    advance: fn(&mut State, &Input, f32, &Ctx),
    lerp: fn(&State, &State, f32) -> State,
  ) -> Self {
    Self {
      state: initial,
      held: Input::default(),
      advance,
      lerp,
      blend: config.blend.clamp(0.0, 1.0),
      ctx: Ctx::default(),
      active: true,
      teleport: None,
    }
  }
}

impl<State: Clone + Debug, Input: Clone + Debug, Ctx> HeldInputPredictor<State, Input, Ctx> {
  /// Treats a disagreement further than `beyond` as a discontinuity to snap
  /// rather than drift to ease.
  ///
  /// Opt-in, and it takes the metric rather than requiring one on `State`, so
  /// applications that do not want this are not made to define a distance. Set
  /// it well above any correction ordinary play produces: everything below it is
  /// eased, and easing across a real teleport draws the entity smoothly through
  /// everything in between.
  pub fn with_teleport(mut self, distance: fn(&State, &State) -> f32, beyond: f32) -> Self {
    self.teleport = Some((distance, beyond));
    self
  }

  /// Sets the input the server is holding for this entity. Call it whenever the
  /// player's intent changes, independently of when it is transmitted: what is
  /// sent is a bandwidth decision, what is integrated is a simulation one.
  pub fn hold(&mut self, input: Input) {
    self.held = input;
  }

  /// What is currently being integrated.
  pub fn held(&self) -> &Input {
    &self.held
  }

  /// Dead reckons one step under the held input. Does nothing while frozen.
  pub fn advance(&mut self, dt_secs: f32) {
    if self.active {
      (self.advance)(&mut self.state, &self.held, dt_secs, &self.ctx);
    }
  }

  /// Where the server's state has probably got to by now, given how old it is.
  ///
  /// An authoritative packet describes the past by one one-way delay, so
  /// correcting straight to it would pull the entity backward by whatever it
  /// travelled in the meantime. Advancing it by its own age under the held input
  /// is what makes the correction target *now*.
  ///
  /// Public so an application can measure the disagreement itself and decide
  /// what to do about it, instead of taking this type's policy.
  pub fn project(&self, authoritative: &State, age_secs: f32) -> State {
    let mut projected = authoritative.clone();
    (self.advance)(&mut projected, &self.held, age_secs, &self.ctx);
    projected
  }

  /// Bends the prediction toward the server, and reports the move.
  ///
  /// `age_secs` is how old `authoritative` is, usually the one-way delay from a
  /// round trip estimate. While frozen this tracks the server exactly, since the
  /// server is not moving the entity and predicting into it would invent a
  /// correction every packet.
  pub fn reconcile(&mut self, authoritative: State, age_secs: f32) -> Correction<State> {
    let seen = self.state.clone();
    if !self.active {
      self.state = authoritative;
      return Correction {
        seen,
        settled: self.state.clone(),
      };
    }

    let target = self.project(&authoritative, age_secs);
    let discontinuous = self
      .teleport
      .is_some_and(|(distance, beyond)| distance(&self.state, &target) > beyond);
    self.state = if discontinuous {
      target
    } else {
      (self.lerp)(&self.state, &target, self.blend)
    };
    Correction {
      seen,
      settled: self.state.clone(),
    }
  }

  /// Moves the entity outright: a spawn, a respawn, a teleport. Not a
  /// correction, so nothing is eased.
  pub fn teleport(&mut self, state: State) {
    self.state = state;
  }

  /// Whether this entity is being simulated. Set it false while the server is
  /// holding it still (dead, stunned, mid respawn); see
  /// [`PredictedPlayer::set_active`](crate::PredictedPlayer::set_active), which
  /// this mirrors.
  pub fn set_active(&mut self, active: bool) {
    self.active = active;
  }

  /// Whether this entity is being simulated.
  pub fn is_active(&self) -> bool {
    self.active
  }

  /// Replaces the world the integration runs against, for a forced entity. See
  /// [`PredictedPlayer::set_context`](crate::PredictedPlayer::set_context).
  pub fn set_context(&mut self, ctx: Ctx) {
    self.ctx = ctx;
  }

  /// The world the integration is running against.
  pub fn context(&self) -> &Ctx {
    &self.ctx
  }

  /// The predicted state. Identical to [`render`](Self::render): the correction
  /// is applied continuously to the state itself, so there is no separate exact
  /// value to preserve. See the [module docs](self).
  pub fn logical(&self) -> &State {
    &self.state
  }

  /// Where to draw the entity.
  pub fn render(&self) -> State {
    self.state.clone()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[derive(Clone, Copy, Debug, Default, PartialEq)]
  struct P(f32);

  /// One shared rule, exactly as both sides should have it.
  fn integrate(p: &mut P, held: &f32, dt: f32, _ctx: &()) {
    p.0 += *held * dt;
  }

  fn lerp(a: &P, b: &P, t: f32) -> P {
    P(a.0 + (b.0 - a.0) * t)
  }

  fn distance(a: &P, b: &P) -> f32 {
    (a.0 - b.0).abs()
  }

  fn predictor(blend: f32) -> HeldInputPredictor<P, f32> {
    HeldInputPredictor::new(P(0.0), HeldInputConfig { blend }, integrate, lerp)
  }

  /// A server that holds the direction and integrates it every tick, which is
  /// the model this primitive exists for.
  #[derive(Default)]
  struct HeldServer {
    pos: f32,
    held: f32,
  }

  impl HeldServer {
    fn step(&mut self, dt: f32) {
      self.pos += self.held * dt;
    }
  }

  #[test]
  fn dead_reckoning_a_held_input_matches_the_server_exactly() {
    // The whole promise: when the client runs the server's rule on the same held
    // input, there is nothing to correct, however rarely input is transmitted.
    let mut me = predictor(0.25);
    let mut server = HeldServer::default();
    me.hold(10.0);
    server.held = 10.0;

    for _ in 0..120 {
      me.advance(1.0 / 60.0);
      server.step(1.0 / 60.0);
    }

    // Both are at the same simulated moment. The packet in flight describes the
    // server as it was one one-way delay ago, which is what the client receives.
    let age = 0.05;
    let sent_at = P(server.pos - 10.0 * age);
    let correction = me.reconcile(sent_at, age);
    assert!(
      distance(&correction.seen, &correction.settled) < 0.001,
      "a shared rule on a held input needs no correction, moved {}",
      distance(&correction.seen, &correction.settled)
    );
  }

  #[test]
  fn a_threshold_snap_sawtooths_where_a_continuous_blend_does_not() {
    // The bug this primitive is shaped to prevent, reproduced next to the fix.
    // A slow systematic drift, corrected two different ways.
    const DRIFT_PER_PACKET: f32 = 6.0;
    const THRESHOLD: f32 = 24.0;

    // Threshold policy: let it accumulate, then close the whole gap at once.
    let mut threshold_pos = 0.0f32;
    let mut server = 0.0f32;
    let mut biggest_snap = 0.0f32;
    for _ in 0..60 {
      server += DRIFT_PER_PACKET;
      if (server - threshold_pos).abs() > THRESHOLD {
        let snap = (server - threshold_pos).abs();
        biggest_snap = biggest_snap.max(snap);
        threshold_pos = server;
      }
    }

    // Continuous policy: the same drift, eased a fraction each packet.
    let mut me = predictor(0.25);
    let mut server = 0.0f32;
    let mut biggest_move = 0.0f32;
    for _ in 0..60 {
      server += DRIFT_PER_PACKET;
      let correction = me.reconcile(P(server), 0.0);
      biggest_move = biggest_move.max(distance(&correction.seen, &correction.settled));
    }

    assert!(biggest_snap > THRESHOLD, "the threshold policy really does snap: {biggest_snap}");
    assert!(
      biggest_move < biggest_snap / 2.0,
      "continuous easing must never move as far in one go: {biggest_move} vs {biggest_snap}"
    );
    assert!(
      distance(me.logical(), &P(server)) < THRESHOLD,
      "and it still keeps up with the server"
    );
  }

  #[test]
  fn a_projection_targets_now_rather_than_the_packets_past() {
    // Correcting straight to an authoritative state pulls the entity backward by
    // however far it travelled while the packet was in flight.
    let mut me = predictor(1.0);
    me.hold(100.0);
    me.advance(0.1); // at 10.0
    assert_eq!(me.logical().0, 10.0);

    // The server says 5.0, but that is 50ms old and the entity is still moving.
    let correction = me.reconcile(P(5.0), 0.05);
    assert_eq!(correction.settled.0, 10.0, "5.0 advanced by 50ms of held input is 10.0");
  }

  #[test]
  fn a_frozen_entity_tracks_the_server_instead_of_predicting_into_it() {
    let mut me = predictor(0.25);
    me.hold(100.0);
    me.set_active(false);

    me.advance(1.0);
    assert_eq!(me.logical().0, 0.0, "a frozen entity does not dead reckon");

    let correction = me.reconcile(P(42.0), 0.05);
    assert_eq!(correction.settled.0, 42.0, "it tracks the held position exactly");
    assert_eq!(me.logical().0, 42.0);

    me.set_active(true);
    me.advance(0.1);
    assert!((me.logical().0 - 52.0).abs() < 1e-3, "and resumes from there");
  }

  #[test]
  fn a_discontinuity_snaps_while_ordinary_drift_eases() {
    let mut me = predictor(0.25).with_teleport(distance, 200.0);

    // Ordinary drift: eased, so it does not arrive in one step.
    let correction = me.reconcile(P(40.0), 0.0);
    assert!(correction.settled.0 < 40.0, "drift is eased, not snapped");

    // A respawn across the level: snapped, because the entity did not travel
    // there and easing would draw it through everything in between.
    let correction = me.reconcile(P(5000.0), 0.0);
    assert_eq!(correction.settled.0, 5000.0, "a discontinuity arrives at once");
  }

  #[test]
  fn a_forced_entity_integrates_its_context() {
    fn with_current(p: &mut P, held: &f32, dt: f32, current: &f32) {
      p.0 += (*held + *current) * dt;
    }
    let mut me: HeldInputPredictor<P, f32, f32> =
      HeldInputPredictor::new(P(0.0), HeldInputConfig::default(), with_current, lerp);
    me.set_context(3.0);
    me.hold(10.0);
    me.advance(1.0);
    assert_eq!(me.logical().0, 13.0, "the world's push is part of the rule");
  }
}
