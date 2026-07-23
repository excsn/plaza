//! A drop-in predicted local player: the whole client-side entity, wired.
//!
//! Prediction, reconciliation, and correction smoothing are separate primitives
//! ([`PredictedEntity`], [`ClientInputBuffer`], [`ErrorSmoother`]) so they can be
//! composed freely. But almost every client composes them the same way, so this
//! bundles them into one type you feed inputs and server packets, and read a
//! render position back from. The primitives stay public for anyone who wants to
//! wire it differently.

use std::fmt::Debug;

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
pub struct PredictedPlayer<State: Clone + Debug, Input: Clone + Debug> {
  predicted: PredictedEntity<State, Input>,
  inputs: ClientInputBuffer<Input, State>,
  smoother: ErrorSmoother<State>,
  next_seq: SequenceNumber,
  apply: fn(&mut State, &Input),
  lerp: fn(&State, &State, f32) -> State,
}

impl<State: Clone + Debug, Input: Clone + Debug> PredictedPlayer<State, Input> {
  pub fn new(
    initial: State,
    config: PlayerConfig,
    apply: fn(&mut State, &Input),
    lerp: fn(&State, &State, f32) -> State,
  ) -> Self {
    Self {
      predicted: PredictedEntity::new(initial),
      inputs: ClientInputBuffer::new(config.input_buffer),
      smoother: ErrorSmoother::new(config.smoothing_secs).with_easing(config.easing),
      next_seq: 0,
      apply,
      lerp,
    }
  }

  /// Applies an input locally (prediction) and records it for replay. Returns the
  /// sequence number to send alongside the input, so the server can acknowledge
  /// it.
  pub fn input(&mut self, input: Input) -> SequenceNumber {
    self.next_seq += 1;
    let seq = self.next_seq;
    self.predicted.apply_local_input_and_predict(&input, seq, &mut self.inputs, &self.apply);
    seq
  }

  /// Folds in the server's authoritative state: snaps the logical state to it,
  /// replays inputs the server had not yet processed, and begins easing the
  /// visible correction.
  ///
  /// `acked_seq` is the last input sequence the server had applied to reach
  /// `authoritative` (an `AuthoritativeStateUpdate` carries both).
  pub fn reconcile(&mut self, authoritative: State, acked_seq: SequenceNumber) {
    // Where the entity is being drawn right now, before the correction moves it.
    let seen = self.render();
    self
      .predicted
      .reconcile_with_server_state(authoritative, acked_seq, &mut self.inputs, &self.apply);
    self.smoother.begin_from(seen);
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

  fn apply(p: &mut P, i: &f32) {
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
