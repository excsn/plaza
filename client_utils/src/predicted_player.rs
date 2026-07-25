//! A drop-in predicted local player: the whole client-side entity, wired.
//!
//! Prediction, reconciliation, and correction smoothing are separate primitives
//! ([`PredictedEntity`], [`ClientInputBuffer`], [`ErrorSmoother`]) so they can be
//! composed freely. But almost every client composes them the same way, so this
//! bundles them into one type you feed inputs and server packets, and read a
//! render position back from. The primitives stay public for anyone who wants to
//! wire it differently.
//!
//! # Before you write an `apply`
//!
//! **It is meant to be the server's step function, not a client copy of it.**
//! Whatever the server does that your copy leaves out does not disappear; it
//! arrives as a correction on every packet, indistinguishable from network
//! jitter and hardest to spot exactly when it matters most. If the rule needs
//! the world to run (gravity, wind, a platform), pass it through
//! [`set_context`](PredictedPlayer::set_context) rather than writing a reduced
//! rule that does not need it.
//!
//! **Predict only what the entity's own input decides.** An ability the server
//! grants subject to a cooldown you cannot see is a permission, not a movement:
//! guessing it means snapping back whenever the guess is wrong. Mispredicting
//! continuous movement is invisible once eased; mispredicting a discrete grant
//! is not.
//!
//! **This is for the discrete input model**, where the server consumes one input
//! per simulation step. If your server holds an input and integrates it every
//! tick, replaying inputs double counts and you want
//! [`HeldInputPredictor`](crate::HeldInputPredictor) instead.

use std::fmt::Debug;

use crate::correction::Correction;
use crate::input_buffer::ClientInputBuffer;
use crate::prediction::PredictedEntity;
use crate::smoothing::{linear, Easing, ErrorSmoother};
use crate::types::SequenceNumber;

/// How a [`PredictedPlayer`] is set up.
#[derive(Debug, Clone, Copy)]
pub struct PlayerConfig {
  /// How many recent inputs to retain for replay. Cover the most inputs that
  /// can be in flight at once (input rate times worst round trip).
  pub input_buffer: usize,
  /// How long a reconciliation correction eases in the render, in seconds.
  /// `0.0` disables smoothing: corrections snap.
  pub smoothing_secs: f32,
  /// The curve the correction eases along (default [`linear`]). Swap in
  /// [`smoothstep`](crate::smoothing::smoothstep) or any `fn(f32) -> f32`.
  pub easing: Easing,
}

impl Default for PlayerConfig {
  fn default() -> Self {
    Self {
      input_buffer: 256,
      smoothing_secs: 0.1,
      easing: linear,
    }
  }
}

/// The local player's entity: predicts on input, reconciles against the server,
/// and eases the correction, exposing both the exact logical state (for further
/// game logic) and a smoothed render state (for drawing).
///
/// The game rule (`apply`) and the render blend (`lerp`) are plain `fn` pointers,
/// so this imposes no `Fn`-closure bounds and stays simple to move around.
///
/// ```ignore
/// let mut me = PredictedPlayer::new(start, PlayerConfig::default(), apply_move, lerp_pos);
///
/// // On input: predict now, send the numbered input.
/// let seq = me.input(mv);
/// send(SequencedClientInput { sequence_number: seq, input_data: mv });
///
/// // On an authoritative packet: reconcile.
/// me.reconcile(packet.state, packet.last_processed_input_seq);
///
/// // Each frame: advance the ease, draw the render state.
/// me.advance(frame_dt_secs);
/// draw(me.render());
/// ```
pub struct PredictedPlayer<State: Clone + Debug, Input: Clone + Debug, Ctx = ()> {
  predicted: PredictedEntity<State, Input>,
  inputs: ClientInputBuffer<Input, State>,
  smoother: ErrorSmoother<State>,
  next_seq: SequenceNumber,
  apply: fn(&mut State, &Input, &Ctx),
  lerp: fn(&State, &State, f32) -> State,
  ctx: Ctx,
  active: bool,
}

impl<State: Clone + Debug, Input: Clone + Debug, Ctx: Default> PredictedPlayer<State, Input, Ctx> {
  pub fn new(
    initial: State,
    config: PlayerConfig,
    apply: fn(&mut State, &Input, &Ctx),
    lerp: fn(&State, &State, f32) -> State,
  ) -> Self {
    Self {
      predicted: PredictedEntity::new(initial),
      inputs: ClientInputBuffer::new(config.input_buffer),
      smoother: ErrorSmoother::new(config.smoothing_secs).with_easing(config.easing),
      next_seq: 0,
      apply,
      lerp,
      ctx: Ctx::default(),
      active: true,
    }
  }
}

impl<State: Clone + Debug, Input: Clone + Debug, Ctx> PredictedPlayer<State, Input, Ctx> {

  /// Applies an input locally (prediction) and records it for replay. Returns the
  /// sequence number to send alongside the input, so the server can acknowledge
  /// it.
  /// Replaces the world the prediction runs against.
  ///
  /// Only needed by a *forced* entity, one the server moves by more than its own
  /// input: gravity, wind, a moving platform, a conveyor. Such a client has to
  /// run the same rule the server runs, and that rule needs to see the world.
  /// Call this whenever a packet refreshes what the client knows.
  ///
  /// The context is held rather than passed per input, so a replay uses the
  /// newest world rather than a snapshot per buffered input. That is a different
  /// approximation, not a strictly better one: the inputs being replayed happened
  /// in the past, under a world that has since moved. It is the cheap one, and
  /// over a replay window of a few frames the difference is usually far smaller
  /// than the force being modelled. An application that needs the exact history
  /// can still carry a snapshot in its `Input` and leave this at `()`.
  pub fn set_context(&mut self, ctx: Ctx) {
    self.ctx = ctx;
  }

  /// The world the prediction is currently running against.
  pub fn context(&self) -> &Ctx {
    &self.ctx
  }

  /// Whether this entity is being simulated at all.
  ///
  /// Set it false while the server is not moving the entity: dead and awaiting a
  /// respawn, stunned, in a cutscene, in a loading screen. A frozen player stops
  /// integrating input and simply tracks the authoritative state, which is what
  /// the server is doing too.
  ///
  /// Without this the client keeps predicting movement for an entity the server
  /// has pinned in place, and every packet reports a disagreement the client
  /// invented. That reads as a correction storm with no cause in the network at
  /// all, and it is one of the more confusing ways for prediction to go wrong.
  pub fn set_active(&mut self, active: bool) {
    self.active = active;
  }

  /// Whether this entity is currently being simulated. See [`set_active`].
  ///
  /// [`set_active`]: Self::set_active
  pub fn is_active(&self) -> bool {
    self.active
  }

  /// Moves the entity outright, with no ease and no replay: a spawn, a respawn,
  /// a teleport.
  ///
  /// The distinction from an ordinary correction is cause, not size. A
  /// correction is a disagreement about a continuous path and must be eased, or
  /// the player sees a jerk. A teleport is not a disagreement at all, and easing
  /// one draws the entity smoothly across the level, through everything in
  /// between, which is worse than the snap it was avoiding. Pending inputs are
  /// dropped because they describe a journey that no longer happened.
  pub fn teleport(&mut self, state: State) {
    self.predicted.current_predicted_state = state.clone();
    self.predicted.last_authoritative_state = state;
    self.inputs.clear();
    self.smoother.reset();
  }

  /// Applies an input locally (prediction) and records it for replay. Returns the
  /// sequence number to send alongside the input, so the server can acknowledge
  /// it.
  ///
  /// While frozen ([`set_active`](Self::set_active)) the input is still numbered,
  /// so the sequence the caller sends stays in step with the server, but nothing
  /// is predicted or recorded: there is no movement to replay over a state the
  /// server is holding still.
  pub fn input(&mut self, input: Input) -> SequenceNumber {
    self.next_seq += 1;
    let seq = self.next_seq;
    if !self.active {
      return seq;
    }
    let (apply, ctx) = (self.apply, &self.ctx);
    self
      .predicted
      .apply_local_input_and_predict(&input, seq, &mut self.inputs, &|s: &mut State, i: &Input| apply(s, i, ctx));
    seq
  }

  /// Folds in the server's authoritative state: snaps the logical state to it,
  /// replays inputs the server had not yet processed, and begins easing the
  /// visible correction.
  ///
  /// `acked_seq` is the last input sequence the server had applied to reach
  /// `authoritative` (an `AuthoritativeStateUpdate` carries both).
  /// Returns what the correction was, as the state being drawn beforehand and
  /// the state settled on afterwards, so a caller that wants to measure its own
  /// prediction error can without this type imposing a metric. Ignore it freely;
  /// it costs a clone of the state either way.
  pub fn reconcile(&mut self, authoritative: State, acked_seq: SequenceNumber) -> Correction<State> {
    // Where the entity is being drawn right now, before the correction moves it.
    let seen = self.render();
    if self.active {
      let (apply, ctx) = (self.apply, &self.ctx);
      self.predicted.reconcile_with_server_state(authoritative, acked_seq, &mut self.inputs, &|s: &mut State, i: &Input| {
        apply(s, i, ctx)
      });
    } else {
      // Frozen: the server is holding this entity still, so there is nothing to
      // replay over its state. Track it exactly and keep the buffer clear, or the
      // first frame after unfreezing would replay inputs from before the freeze.
      self.predicted.current_predicted_state = authoritative.clone();
      self.predicted.last_authoritative_state = authoritative;
      self.predicted.last_server_acknowledged_input_seq = acked_seq;
      self.inputs.clear();
    }
    self.smoother.begin_from(seen.clone());
    Correction {
      seen,
      settled: self.predicted.current_predicted_state.clone(),
    }
  }

  /// Progresses the correction ease by one frame.
  pub fn advance(&mut self, dt_secs: f32) {
    self.smoother.advance(dt_secs);
  }

  /// Where to draw the entity: the prediction, eased through recent corrections.
  pub fn render(&self) -> State {
    self.smoother.sample(&self.predicted.current_predicted_state, self.lerp)
  }

  /// The exact predicted state, for further game logic. Never smoothed.
  pub fn logical(&self) -> &State {
    &self.predicted.current_predicted_state
  }

  /// The last state the server confirmed, for a ghost overlay or an error readout.
  pub fn authoritative(&self) -> &State {
    &self.predicted.last_authoritative_state
  }

  /// The most recent input sequence produced by [`input`](Self::input).
  pub fn latest_seq(&self) -> SequenceNumber {
    self.next_seq
  }

  /// The last input sequence the server has acknowledged.
  pub fn acked_seq(&self) -> SequenceNumber {
    self.predicted.last_server_acknowledged_input_seq
  }

  /// How many sent inputs are still awaiting acknowledgement, i.e. what a
  /// reconciliation replays.
  pub fn unacked_count(&self) -> usize {
    self.inputs.get_unacknowledged_inputs(self.acked_seq()).count()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[derive(Clone, Debug, PartialEq)]
  struct P(f32);

  fn apply(p: &mut P, i: &f32, _ctx: &()) {
    p.0 += *i;
  }

  fn lerp(a: &P, b: &P, t: f32) -> P {
    P(a.0 + (b.0 - a.0) * t)
  }

  fn player(smoothing_secs: f32) -> PredictedPlayer<P, f32> {
    PredictedPlayer::new(
      P(0.0),
      PlayerConfig {
        input_buffer: 64,
        smoothing_secs,
        ..PlayerConfig::default()
      },
      apply,
      lerp,
    )
  }

  #[test]
  fn predicting_moves_the_logical_state() {
    let mut me = player(0.0);
    me.input(1.0);
    me.input(1.0);
    assert_eq!(me.logical().0, 2.0);
  }

  #[test]
  fn reconciliation_replays_unacknowledged_inputs() {
    let mut me = player(0.0);
    let s1 = me.input(1.0); // logical 1
    me.input(1.0); // logical 2, this one still unacked

    // The server has only processed the first input: state 1 as of s1.
    me.reconcile(P(1.0), s1);

    // Snap to 1, then replay the still-unacked second input: back to 2.
    assert_eq!(me.logical().0, 2.0);
    assert_eq!(me.unacked_count(), 1);
  }

  #[test]
  fn a_correction_eases_the_render_but_not_the_logical() {
    let mut me = player(0.1);
    let s = me.input(10.0); // mispredict to 10; render idle, so it draws at 10

    // The server disagrees: the true state is 0.
    me.reconcile(P(0.0), s);
    assert_eq!(me.logical().0, 0.0, "logical snaps to authority immediately");
    assert!((me.render().0 - 10.0).abs() < 1e-3, "render starts where the eye was");

    me.advance(0.05);
    assert!((me.render().0 - 5.0).abs() < 0.2, "render eases halfway");

    me.advance(0.05);
    assert!((me.render().0 - 0.0).abs() < 1e-3, "render arrives at the logical state");
  }

  #[test]
  fn a_forced_entity_predicts_the_force_from_its_context() {
    // The lesson a real game paid for: an entity the server moves by more than
    // its own input has to run the same rule, and that rule needs the world. With
    // nowhere to put the world, a client writes a second, lesser rule and drifts
    // by the whole size of the force it left out.
    fn apply_with_wind(p: &mut P, i: &f32, wind: &f32) {
      p.0 += *i + *wind;
    }
    let mut me: PredictedPlayer<P, f32, f32> = PredictedPlayer::new(
      P(0.0),
      PlayerConfig { input_buffer: 64, smoothing_secs: 0.0, ..PlayerConfig::default() },
      apply_with_wind,
      lerp,
    );
    me.set_context(0.5);
    me.input(1.0);
    me.input(1.0);
    assert_eq!(me.logical().0, 3.0, "each step carries the input plus the wind");

    // The server agrees, because it ran the same rule. Nothing to correct.
    let correction = me.reconcile(P(3.0), me.latest_seq());
    assert_eq!(correction.seen, correction.settled, "a matching rule needs no correction");
  }

  #[test]
  fn a_frozen_entity_stops_predicting_instead_of_inventing_corrections() {
    // A server that is holding an entity still (dead, stunned, mid respawn) will
    // keep reporting the same position. A client that keeps integrating input
    // into it manufactures a correction every single packet, out of nothing.
    let mut me = player(0.0);
    me.input(1.0);
    assert_eq!(me.logical().0, 1.0);

    me.set_active(false);
    me.input(1.0);
    me.input(1.0);
    assert_eq!(me.logical().0, 1.0, "a frozen entity does not move on input");

    let correction = me.reconcile(P(1.0), me.latest_seq());
    assert_eq!(correction.seen, correction.settled, "and so it never disagrees with the server");
    assert_eq!(me.unacked_count(), 0, "nothing is queued for replay while frozen");

    // Unfrozen, it picks straight back up without replaying the frozen inputs.
    me.set_active(true);
    me.input(1.0);
    assert_eq!(me.logical().0, 2.0);
  }

  #[test]
  fn a_teleport_snaps_and_drops_the_journey() {
    // A correction is a disagreement about a path and must be eased. A teleport
    // is not a disagreement at all: easing it draws the entity smoothly across
    // everything in between.
    let mut me = player(0.5);
    me.input(1.0);
    me.reconcile(P(50.0), 0); // a big correction, now easing
    assert!(me.render().0 < 50.0, "an ordinary correction eases");

    me.teleport(P(900.0));
    assert_eq!(me.render().0, 900.0, "a teleport is visible immediately");
    assert_eq!(me.logical().0, 900.0);
    assert_eq!(me.unacked_count(), 0, "pending inputs described a journey that did not happen");
  }

  #[test]
  fn reconcile_reports_what_it_corrected() {
    let mut me = player(0.0);
    me.input(1.0);
    me.input(1.0);
    // The server only got the first input, and disagrees about where it led.
    let correction = me.reconcile(P(10.0), 1);
    assert_eq!(correction.seen.0, 2.0, "where it was being drawn");
    assert_eq!(correction.settled.0, 11.0, "10 authoritative, replaying the unacked second input");
  }

  #[test]
  fn overflowing_the_input_buffer_does_not_panic_or_go_wrong_within_the_window() {
    // A tiny buffer, then far more unacked inputs than it holds.
    let mut me = PredictedPlayer::new(
      P(0.0),
      PlayerConfig {
        input_buffer: 4,
        smoothing_secs: 0.0,
        ..PlayerConfig::default()
      },
      apply,
      lerp,
    );
    for _ in 0..20 {
      me.input(1.0);
    }
    assert_eq!(me.logical().0, 20.0, "prediction advanced through all inputs");

    // Reconcile to an ack still inside the retained window: replay is exact.
    let latest = me.latest_seq();
    me.reconcile(P(17.0), latest - 3); // server state 17 after processing seq (latest-3)=17
    assert_eq!(me.logical().0, 20.0, "17 authoritative, replay 18/19/20 back to 20");
    assert!(me.logical().0.is_finite());
  }

  #[test]
  fn reconciling_with_a_future_ack_snaps_and_clears() {
    let mut me = player(0.0);
    me.input(1.0);
    me.input(1.0);
    // The server claims to have processed more than we have sent (shouldn't happen,
    // but must not panic): everything acknowledges, nothing replays.
    me.reconcile(P(42.0), 9999);
    assert_eq!(me.logical().0, 42.0);
    assert_eq!(me.unacked_count(), 0);
  }

  #[test]
  fn zero_smoothing_renders_the_logical_at_once() {
    let mut me = player(0.0);
    let s = me.input(10.0);
    me.reconcile(P(0.0), s);
    assert_eq!(me.render().0, 0.0, "no ease, render is the logical state");
  }
}
