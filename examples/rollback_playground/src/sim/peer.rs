//! One peer, built from `plaza_client_utils`.
//!
//! It is a thin wrapper over a [`RollbackSession`]: the session is already the
//! whole rollback loop, so this only pins down which player index is *local* to
//! this peer and turns the demo's toggles into policy the session leaves to the
//! app, predict-vs-wait, and rollback on or off.

use plaza_client_utils::rollback::{Frame, RollbackConfig, RollbackSession};

use crate::sim::types::{step, Controls, GameState, Input, Vec2, NEUTRAL};

/// The furthest back a peer can roll: four seconds at 60 fps, well past any
/// latency the sliders allow.
const MAX_ROLLBACK: usize = 240;

pub struct Peer {
  session: RollbackSession<GameState, Input>,
  /// Which player this peer controls directly; the other is remote.
  local: usize,
}

impl Peer {
  pub fn new(local: usize) -> Self {
    let config = RollbackConfig { max_rollback_frames: MAX_ROLLBACK };
    Self {
      session: RollbackSession::new(GameState::start(), vec![NEUTRAL, NEUTRAL], config, step),
      local,
    }
  }

  fn remote(&self) -> usize {
    1 - self.local
  }

  pub fn local_player(&self) -> usize {
    self.local
  }

  /// Supplies this peer's own input for the frame it is about to run.
  pub fn queue_local(&mut self, input: Input) {
    self.session.queue_local_input(self.local, input);
  }

  /// Folds in a remote input packet: every frame it carries, so a redundant tail
  /// backfills a frame an earlier packet lost.
  pub fn deliver(&mut self, inputs: &[(Frame, Input)]) {
    let remote = self.remote();
    for (frame, input) in inputs {
      self.session.confirm_remote_input(remote, *frame, *input);
    }
  }

  /// Advances one frame under the current policy. Returns whether it advanced:
  /// a delay-based peer (prediction off) stalls until the remote input is in,
  /// which is what makes it hitch under latency.
  pub fn advance(&mut self, controls: &Controls) -> bool {
    self.session.set_rollback_enabled(controls.rollback);
    if controls.predict || self.session.is_frame_confirmed(self.session.current_frame()) {
      self.session.advance_frame();
      true
    } else {
      false
    }
  }

  pub fn current_frame(&self) -> Frame {
    self.session.current_frame()
  }

  /// The present this peer renders: both boxes, the remote one predicted.
  pub fn state(&self) -> &GameState {
    self.session.state()
  }

  /// The saved world at the start of `frame`, for the cross-peer in-sync check.
  pub fn state_at(&self, frame: Frame) -> Option<GameState> {
    self.session.state_at(frame)
  }

  /// The newest frame this peer has the remote player's real input for.
  pub fn remote_confirmed_frame(&self) -> Option<Frame> {
    self.session.confirmed_frame(self.remote())
  }

  /// Where the remote box truly is as far as confirmed inputs prove, drawn as a
  /// ghost behind its predicted position. The gap between the two is exactly the
  /// span a rollback would re-simulate if the next input disagrees.
  pub fn remote_ghost(&self) -> Option<Vec2> {
    let confirmed = self.remote_confirmed_frame()?;
    // The state after the last confirmed remote input incorporates it.
    let after = self.state_at(confirmed + 1).or_else(|| self.state_at(confirmed))?;
    Some(after.boxes[self.remote()])
  }

  pub fn last_rollback_frames(&self) -> usize {
    self.session.last_rollback_frames()
  }

  pub fn max_rollback_frames(&self) -> usize {
    self.session.max_rollback_frames()
  }

  pub fn rollback_count(&self) -> u64 {
    self.session.rollback_count()
  }

  pub fn prediction_horizon(&self) -> usize {
    self.session.prediction_horizon()
  }
}
