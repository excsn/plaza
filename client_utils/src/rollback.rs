//! Rollback netcode: the deterministic-lockstep building blocks.
//!
//! The rest of this crate serves *server-authoritative* play, where the server
//! decides and the client predicts its own entity. Rollback is the other family:
//! peers run the *same deterministic simulation*, exchange only inputs, and stay
//! identical frame for frame. The catch is latency, a peer cannot wait for a
//! remote input and still feel responsive, so it **predicts** the missing input
//! (usually: repeat the last one), simulates ahead, and when the real input
//! arrives and disagrees, **rolls back** to the frame it went wrong and
//! re-simulates to the present with the correction. Determinism is what makes the
//! re-simulation land on the same state the remote peer already has.
//!
//! Three pieces, smallest first:
//!
//! - [`StateHistory`]: a frame-indexed ring of whole-world snapshots. Rollback
//!   restores one of these and re-simulates forward. Pure save/restore by frame,
//!   no interpolation (unlike a server's [`crate::interpolation::SnapshotBuffer`],
//!   which blends between times).
//! - [`InputTimeline`]: the inputs known for one source, with the missing frames
//!   predicted by repeating the last confirmed one.
//! - [`RollbackSession`]: the bundle that wires them together and runs the loop,
//!   analogous to [`crate::PredictedPlayer`] for the authoritative model. The
//!   deterministic step is a plain `fn` pointer you supply, so the simulation
//!   rules stay entirely yours.
//!
//! Everything here is pure logic: no clock, no transport, no float assumptions.
//! You drive it a frame at a time and feed it inputs as they arrive.

use std::collections::VecDeque;
use std::fmt::Debug;

/// A logical simulation frame. Rollback counts in fixed frames, not wall time:
/// two peers agree on "frame 900", never on a millisecond.
pub type Frame = u64;

/// A frame-indexed ring of whole-world state snapshots.
///
/// Rollback saves the state at the start of every frame and, on a misprediction,
/// restores the one at the frame that went wrong. Frames are contiguous: you save
/// `f`, then `f + 1`, and so on; re-simulation saves the same frames again, which
/// overwrites in place. Only the most recent `capacity` frames are kept, which is
/// the maximum distance you can ever roll back.
#[derive(Debug, Clone)]
pub struct StateHistory<State: Clone> {
  ring: VecDeque<State>,
  base_frame: Frame,
  capacity: usize,
  resets: u64,
}

impl<State: Clone> StateHistory<State> {
  /// Keeps at most `capacity` frames of history.
  ///
  /// # Panics
  /// Panics if `capacity` is 0.
  pub fn new(capacity: usize) -> Self {
    if capacity == 0 {
      panic!("StateHistory capacity must be greater than 0");
    }
    Self {
      ring: VecDeque::with_capacity(capacity),
      base_frame: 0,
      capacity,
      resets: 0,
    }
  }

  /// Records the state at `frame`.
  ///
  /// The intended use is contiguous: append `frame == latest + 1`, or overwrite a
  /// frame already inside the window (re-simulation does this). A save that skips
  /// ahead of the window resets it, so the buffer never holds a gap.
  pub fn save(&mut self, frame: Frame, state: State) {
    if self.ring.is_empty() {
      self.base_frame = frame;
      self.ring.push_back(state);
      return;
    }

    let end = self.base_frame + self.ring.len() as u64; // one past the newest
    if frame == end {
      self.ring.push_back(state);
      while self.ring.len() > self.capacity {
        self.ring.pop_front();
        self.base_frame += 1;
      }
    } else if frame >= self.base_frame && frame < end {
      let idx = (frame - self.base_frame) as usize;
      self.ring[idx] = state;
    } else {
      self.resets += 1;
      tracing::warn!(?frame, base = self.base_frame, len = self.ring.len(), "StateHistory: non-contiguous save, resetting the window");
      self.ring.clear();
      self.base_frame = frame;
      self.ring.push_back(state);
    }
  }

  /// The state saved at `frame`, if still retained. `None` if it was evicted or
  /// never saved.
  pub fn restore(&self, frame: Frame) -> Option<State> {
    if self.ring.is_empty() || frame < self.base_frame {
      return None;
    }
    let idx = (frame - self.base_frame) as usize;
    self.ring.get(idx).cloned()
  }

  /// The oldest frame still retained, or `None` if empty.
  pub fn oldest_frame(&self) -> Option<Frame> {
    (!self.ring.is_empty()).then_some(self.base_frame)
  }

  /// The newest frame saved, or `None` if empty.
  pub fn latest_frame(&self) -> Option<Frame> {
    (!self.ring.is_empty()).then(|| self.base_frame + self.ring.len() as u64 - 1)
  }

  pub fn len(&self) -> usize {
    self.ring.len()
  }

  pub fn is_empty(&self) -> bool {
    self.ring.is_empty()
  }

  pub fn clear(&mut self) {
    self.ring.clear();
    self.base_frame = 0;
  }

  /// How many saves fell outside the window and reset it.
  ///
  /// Rollback assumes frames are saved contiguously, so this should stay zero for
  /// the whole life of a session. Non-zero means the window was thrown away and
  /// rebuilt from one frame, which silently shortens how far back the session can
  /// roll: a correction that arrives next frame finds nothing to restore.
  pub fn resets(&self) -> u64 {
    self.resets
  }
}

/// The inputs known for one input source (one player), by frame, with the gaps
/// predicted.
///
/// A confirmed input is one the source actually produced; an unconfirmed frame is
/// **predicted** by repeating the last confirmed input at or before it. That is
/// the standard rollback guess, and it is right whenever a player holds a
/// direction, which is most of the time. [`RollbackSession`] compares a later
/// confirmation against what it predicted to decide whether to roll back.
#[derive(Debug, Clone)]
pub struct InputTimeline<Input: Clone + Debug> {
  slots: VecDeque<Option<Input>>,
  base_frame: Frame,
  last_confirmed: Option<Frame>,
  capacity: usize,
}

impl<Input: Clone + Debug> InputTimeline<Input> {
  /// Retains inputs across at most `capacity` frames.
  ///
  /// # Panics
  /// Panics if `capacity` is 0.
  pub fn new(capacity: usize) -> Self {
    if capacity == 0 {
      panic!("InputTimeline capacity must be greater than 0");
    }
    Self {
      slots: VecDeque::with_capacity(capacity),
      base_frame: 0,
      last_confirmed: None,
      capacity,
    }
  }

  /// Records the real input the source produced for `frame`.
  ///
  /// Frames may arrive out of order (a resent input can fill a gap left by a lost
  /// packet); any missing frames between the window and `frame` are held as gaps
  /// until they too are confirmed. A frame older than the retained window is
  /// dropped, it is already past the rollback horizon.
  pub fn confirm(&mut self, frame: Frame, input: Input) {
    if self.slots.is_empty() {
      self.base_frame = frame;
      self.slots.push_back(Some(input));
    } else if frame < self.base_frame {
      return; // older than anything retained: beyond the horizon
    } else {
      let end = self.base_frame + self.slots.len() as u64;
      if frame < end {
        self.slots[(frame - self.base_frame) as usize] = Some(input);
      } else {
        while self.base_frame + (self.slots.len() as u64) < frame {
          self.slots.push_back(None);
        }
        self.slots.push_back(Some(input));
      }
      while self.slots.len() > self.capacity {
        self.slots.pop_front();
        self.base_frame += 1;
      }
    }
    self.last_confirmed = Some(self.last_confirmed.map_or(frame, |c| c.max(frame)));
  }

  /// The confirmed input at `frame`, or `None` if that frame is unconfirmed
  /// (predicted) or outside the window.
  pub fn confirmed_at(&self, frame: Frame) -> Option<&Input> {
    if self.slots.is_empty() || frame < self.base_frame {
      return None;
    }
    let idx = (frame - self.base_frame) as usize;
    self.slots.get(idx).and_then(|s| s.as_ref())
  }

  /// The most recent confirmed input at or before `frame`: the basis for
  /// predicting `frame` when it is not itself confirmed.
  pub fn last_confirmed_at_or_before(&self, frame: Frame) -> Option<&Input> {
    if self.slots.is_empty() || frame < self.base_frame {
      return None;
    }
    let end = self.base_frame + self.slots.len() as u64;
    let mut f = frame.min(end - 1);
    loop {
      let idx = (f - self.base_frame) as usize;
      if let Some(input) = self.slots[idx].as_ref() {
        return Some(input);
      }
      if f == self.base_frame {
        return None;
      }
      f -= 1;
    }
  }

  /// The newest frame ever confirmed for this source, or `None` if none has been.
  pub fn last_confirmed_frame(&self) -> Option<Frame> {
    self.last_confirmed
  }
}

/// How a [`RollbackSession`] is set up.
#[derive(Debug, Clone, Copy)]
pub struct RollbackConfig {
  /// The furthest back the session can roll, in frames. It bounds the state and
  /// input history retained, so it must comfortably exceed the worst prediction
  /// horizon (round-trip latency in frames). Default 240 (four seconds at 60 fps).
  pub max_rollback_frames: usize,
}

impl Default for RollbackConfig {
  fn default() -> Self {
    Self { max_rollback_frames: 240 }
  }
}

/// The default input predictor: repeat the last confirmed input unchanged.
///
/// This is right whenever a player holds their input steady, which dominates most
/// games, and it is what a session uses unless [`RollbackSession::with_predictor`]
/// supplies another rule.
pub fn repeat_last_input<Input: Clone>(last: &Input, _frame: Frame) -> Input {
  last.clone()
}

/// The whole rollback loop for one peer, wired.
///
/// It owns a [`StateHistory`], an [`InputTimeline`] per player, and the current
/// frame, and drives the predict / detect / rollback / re-simulate cycle against
/// a deterministic step you supply. The primitives stay public for anyone who
/// wants to wire the loop differently; this is the ready-made path, the rollback
/// counterpart to [`crate::PredictedPlayer`].
///
/// Each peer runs its own session and calls its local player index the "local"
/// one; the two are otherwise identical, which is the point, both re-simulate to
/// the same state from the same inputs.
///
/// ```ignore
/// // Two players; the deterministic step advances the shared world one frame.
/// let mut session = RollbackSession::new(initial_world, vec![NEUTRAL, NEUTRAL],
///                                        RollbackConfig::default(), step);
///
/// // Each frame: feed the local input, fold in any remote inputs that arrived,
/// // then advance (which rolls back first if a remote input disproved a guess).
/// session.queue_local_input(LOCAL, my_input);
/// for (frame, input) in inbox.drain() { session.confirm_remote_input(REMOTE, frame, input); }
/// session.advance_frame();
/// draw(session.state());
/// ```
pub struct RollbackSession<State: Clone + Debug, Input: Clone + Debug + PartialEq> {
  state_history: StateHistory<State>,
  /// What input was actually fed to each player for each simulated frame, so a
  /// later confirmation can be checked against the guess that was used.
  used: Vec<StateHistory<Input>>,
  timelines: Vec<InputTimeline<Input>>,
  /// The neutral input assumed for a player before it has confirmed anything.
  neutral: Vec<Input>,

  current_state: State,
  head_frame: Frame,

  advance: fn(&State, &[Input]) -> State,
  predictor: fn(&Input, Frame) -> Input,

  rollback_enabled: bool,
  earliest_incorrect: Option<Frame>,
  last_rollback_len: usize,
  max_rollback_len: usize,
  rollback_count: u64,
}

impl<State: Clone + Debug, Input: Clone + Debug + PartialEq> RollbackSession<State, Input> {
  /// Creates a session over `neutral_inputs.len()` players, starting from
  /// `initial_state`. `neutral_inputs[p]` is the input assumed for player `p`
  /// before any of its inputs are known (typically "no input"). `advance` is the
  /// deterministic step: same state and inputs in, same state out, every time and
  /// on every peer, which is what rollback rests on.
  pub fn new(initial_state: State, neutral_inputs: Vec<Input>, config: RollbackConfig, advance: fn(&State, &[Input]) -> State) -> Self {
    let cap = config.max_rollback_frames.max(1);
    let players = neutral_inputs.len();
    Self {
      state_history: StateHistory::new(cap),
      used: (0..players).map(|_| StateHistory::new(cap)).collect(),
      timelines: (0..players).map(|_| InputTimeline::new(cap)).collect(),
      neutral: neutral_inputs,
      current_state: initial_state,
      head_frame: 0,
      advance,
      predictor: repeat_last_input,
      rollback_enabled: true,
      earliest_incorrect: None,
      last_rollback_len: 0,
      max_rollback_len: 0,
      rollback_count: 0,
    }
  }

  /// Replaces the input predictor (default: [`repeat_last_input`]). A predictor
  /// takes the last confirmed input and the frame being predicted and returns the
  /// guess for that frame.
  pub fn with_predictor(mut self, predictor: fn(&Input, Frame) -> Input) -> Self {
    self.predictor = predictor;
    self
  }

  pub fn num_players(&self) -> usize {
    self.timelines.len()
  }

  /// The next frame to be simulated. [`state`](Self::state) is the world at the
  /// start of this frame, i.e. after every frame before it.
  pub fn current_frame(&self) -> Frame {
    self.head_frame
  }

  /// The world as it stands now: the present the peer renders. Includes the effect
  /// of every predicted input still awaiting confirmation.
  pub fn state(&self) -> &State {
    &self.current_state
  }

  /// The world at the start of `frame`, if still retained. This is the *saved*
  /// state, so for a fully-confirmed frame it is identical on every peer, that
  /// equality is the determinism guarantee, and comparing two peers here is how a
  /// demo shows they are in sync. Returns the present for the current frame.
  pub fn state_at(&self, frame: Frame) -> Option<State> {
    if frame == self.head_frame {
      return Some(self.current_state.clone());
    }
    self.state_history.restore(frame)
  }

  /// Turns rollback on or off (on by default). With it off the session still
  /// predicts and advances, but never restores or re-simulates: it trusts every
  /// guess permanently. That is not a way to ship, predictions that are never
  /// corrected drift a peer out of sync, but it isolates what rollback buys, and
  /// it is the mechanism a delay-based front end disables when it waits for inputs
  /// instead of predicting them.
  pub fn set_rollback_enabled(&mut self, enabled: bool) {
    self.rollback_enabled = enabled;
  }

  /// Supplies the local player's input for the current frame. Local inputs are
  /// known before their frame runs, so they are never mispredicted. Call once per
  /// frame before [`advance_frame`](Self::advance_frame).
  pub fn queue_local_input(&mut self, player: usize, input: Input) {
    self.timelines[player].confirm(self.head_frame, input);
  }

  /// Folds in a remote input that has arrived for a past or current `frame`. If it
  /// contradicts the guess already used for an *already-simulated* frame, the
  /// session marks that frame for rollback on the next
  /// [`advance_frame`](Self::advance_frame).
  pub fn confirm_remote_input(&mut self, player: usize, frame: Frame, input: Input) {
    self.timelines[player].confirm(frame, input.clone());

    if frame < self.head_frame
      && let Some(used) = self.used[player].restore(frame)
      && used != input
    {
      self.earliest_incorrect = Some(self.earliest_incorrect.map_or(frame, |cur| cur.min(frame)));
    }
  }

  /// Advances the simulation by one frame: first rolls back and re-simulates if a
  /// confirmation disproved a guess, then simulates the current frame, predicting
  /// any input not yet known.
  pub fn advance_frame(&mut self) {
    self.resolve_rollback();
    let f = self.head_frame;
    self.simulate_frame(f);
    self.head_frame += 1;
  }

  /// Whether every player's input for `frame` is confirmed (none predicted). A
  /// delay-based peer waits for this before advancing; a rollback peer ignores it
  /// and predicts. Which to do is the app's policy, so this only reports.
  pub fn is_frame_confirmed(&self, frame: Frame) -> bool {
    self.timelines.iter().all(|t| t.confirmed_at(frame).is_some())
  }

  /// The newest frame this player's input is confirmed through, or `None`.
  pub fn confirmed_frame(&self, player: usize) -> Option<Frame> {
    self.timelines[player].last_confirmed_frame()
  }

  /// How many frames the present is running ahead of the least-confirmed player:
  /// the depth of prediction currently exposed to a rollback. Zero when every
  /// input is known up to the last simulated frame.
  pub fn prediction_horizon(&self) -> usize {
    if self.head_frame == 0 {
      return 0;
    }
    let last_simulated = self.head_frame - 1;
    (0..self.timelines.len())
      .map(|p| match self.timelines[p].last_confirmed_frame() {
        Some(c) if c >= last_simulated => 0,
        Some(c) => (last_simulated - c) as usize,
        None => self.head_frame as usize,
      })
      .max()
      .unwrap_or(0)
  }

  /// Frames re-simulated by the most recent [`advance_frame`](Self::advance_frame)
  /// (0 if it did not roll back).
  pub fn last_rollback_frames(&self) -> usize {
    self.last_rollback_len
  }

  /// The deepest rollback seen so far, in frames.
  pub fn max_rollback_frames(&self) -> usize {
    self.max_rollback_len
  }

  /// How many times the session has rolled back.
  pub fn rollback_count(&self) -> u64 {
    self.rollback_count
  }

  /// Restores the earliest mispredicted frame and re-simulates to the present.
  fn resolve_rollback(&mut self) {
    self.last_rollback_len = 0;
    let Some(mut earliest) = self.earliest_incorrect.take() else {
      return;
    };
    if !self.rollback_enabled {
      return; // guess kept, never corrected: the "why rollback" comparison
    }
    if earliest >= self.head_frame {
      return; // nothing simulated yet went wrong
    }
    // Clamp to what is still retained: a correction older than the history can no
    // longer be applied exactly. Re-simulating from the oldest kept frame is the
    // closest the session can get, and keeps it from diverging further.
    match self.state_history.oldest_frame() {
      Some(oldest) if earliest < oldest => earliest = oldest,
      None => return,
      _ => {}
    }
    let Some(restored) = self.state_history.restore(earliest) else {
      return;
    };

    let target = self.head_frame; // re-simulate [earliest, target)
    self.current_state = restored;
    let mut f = earliest;
    while f < target {
      self.simulate_frame(f);
      f += 1;
    }

    let len = (target - earliest) as usize;
    self.last_rollback_len = len;
    self.max_rollback_len = self.max_rollback_len.max(len);
    self.rollback_count += 1;
  }

  /// Simulates one frame from `current_state`: saves the pre-state, gathers each
  /// player's input (confirmed or predicted), records what it used, and steps.
  fn simulate_frame(&mut self, frame: Frame) {
    self.state_history.save(frame, self.current_state.clone());

    let mut inputs = Vec::with_capacity(self.timelines.len());
    for p in 0..self.timelines.len() {
      let input = self.input_for(p, frame);
      self.used[p].save(frame, input.clone());
      inputs.push(input);
    }
    self.current_state = (self.advance)(&self.current_state, &inputs);
  }

  /// Player `p`'s input for `frame`: the confirmed value if known, otherwise the
  /// predictor applied to the last confirmed input (or the neutral input if the
  /// player has confirmed nothing yet).
  fn input_for(&self, p: usize, frame: Frame) -> Input {
    if let Some(confirmed) = self.timelines[p].confirmed_at(frame) {
      return confirmed.clone();
    }
    let basis = self.timelines[p].last_confirmed_at_or_before(frame).cloned().unwrap_or_else(|| self.neutral[p].clone());
    (self.predictor)(&basis, frame)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  // A tiny deterministic world: each player's position is an integer, moved by an
  // integer input. Integers keep re-simulation exactly comparable, no float drift.
  #[derive(Clone, Copy, Debug, PartialEq)]
  struct World {
    pos: [i64; 2],
  }

  #[derive(Clone, Copy, Debug, PartialEq)]
  struct In(i64);

  const NEUTRAL: In = In(0);

  fn step(state: &World, inputs: &[In]) -> World {
    let mut next = *state;
    next.pos[0] += inputs[0].0;
    next.pos[1] += inputs[1].0;
    next
  }

  fn session() -> RollbackSession<World, In> {
    RollbackSession::new(World { pos: [0, 0] }, vec![NEUTRAL, NEUTRAL], RollbackConfig { max_rollback_frames: 64 }, step)
  }

  // A ground-truth simulation with every input known up front, for comparison.
  fn ground_truth(inputs0: &[In], inputs1: &[In]) -> World {
    let mut w = World { pos: [0, 0] };
    for f in 0..inputs0.len() {
      w = step(&w, &[inputs0[f], inputs1[f]]);
    }
    w
  }

  #[test]
  fn state_history_saves_restores_and_evicts_by_frame() {
    let mut h = StateHistory::new(3);
    h.save(0, 10);
    h.save(1, 11);
    h.save(2, 12);
    assert_eq!(h.restore(1), Some(11));
    assert_eq!(h.oldest_frame(), Some(0));
    assert_eq!(h.latest_frame(), Some(2));

    h.save(3, 13); // evicts frame 0
    assert_eq!(h.restore(0), None);
    assert_eq!(h.oldest_frame(), Some(1));
    assert_eq!(h.restore(3), Some(13));
  }

  #[test]
  fn state_history_overwrites_a_frame_in_place() {
    let mut h = StateHistory::new(4);
    h.save(0, 1);
    h.save(1, 2);
    h.save(1, 99); // re-simulation re-saves the same frame
    assert_eq!(h.restore(1), Some(99));
    assert_eq!(h.latest_frame(), Some(1), "overwriting does not extend the window");
  }

  #[test]
  fn a_non_contiguous_save_is_counted_because_it_shortens_the_reach() {
    // Rollback assumes contiguous saves, so this should stay zero for a whole
    // session. Non-zero means the window was thrown away and rebuilt from one
    // frame, and a correction arriving next frame finds nothing to restore.
    let mut h = StateHistory::new(4);
    h.save(0, 1);
    h.save(1, 2);
    assert_eq!(h.resets(), 0);
    h.save(50, 3);
    assert_eq!(h.resets(), 1);
    assert_eq!(h.oldest_frame(), Some(50), "and the reach is now one frame");
    assert_eq!(h.restore(1), None);
  }

  #[test]
  fn input_timeline_predicts_the_last_confirmed() {
    let mut t = InputTimeline::new(8);
    t.confirm(0, In(5));
    t.confirm(1, In(5));
    assert_eq!(t.confirmed_at(1), Some(&In(5)));
    assert_eq!(t.confirmed_at(2), None, "frame 2 is unconfirmed");
    // The basis for predicting frame 5 is the last confirmed at or before it.
    assert_eq!(t.last_confirmed_at_or_before(5), Some(&In(5)));
    assert_eq!(t.last_confirmed_frame(), Some(1));
  }

  #[test]
  fn a_correct_prediction_never_rolls_back() {
    // The remote holds In(2) the whole time. Once repeat-last has a basis to
    // repeat, it predicts the held input exactly, so lagging confirmations never
    // contradict a guess. The basis is the first confirmed input, so frame 0 is
    // known before it runs (both peers start from a known first input).
    let mut s = session();
    let inputs = [In(2); 10];
    s.confirm_remote_input(1, 0, inputs[0]);
    for f in 0..10u64 {
      s.queue_local_input(0, In(1));
      // Confirm the rest three frames late.
      if f >= 3 {
        s.confirm_remote_input(1, f - 3, inputs[(f - 3) as usize]);
      }
      s.advance_frame();
    }
    assert_eq!(s.rollback_count(), 0, "a held remote input is predicted exactly");
    assert_eq!(s.last_rollback_frames(), 0);
  }

  #[test]
  fn a_wrong_prediction_rolls_back_and_lands_on_the_truth() {
    // The remote changes direction at frame 4, which repeat-last cannot foresee.
    let remote: Vec<In> = (0..10).map(|f| if f < 4 { In(1) } else { In(-3) }).collect();
    let local = vec![In(1); 10];

    let mut s = session();
    for f in 0..10u64 {
      s.queue_local_input(0, local[f as usize]);
      // The remote input for frame f arrives two frames late.
      if f >= 2 {
        let past = f - 2;
        s.confirm_remote_input(1, past, remote[past as usize]);
      }
      s.advance_frame();
    }
    // Drain the last in-flight confirmations and let it settle.
    for f in 8..10u64 {
      s.confirm_remote_input(1, f, remote[f as usize]);
    }
    s.resolve_rollback();

    assert!(s.rollback_count() > 0, "the direction change forced at least one rollback");
    // After every input is known, the re-simulated present equals the world that
    // had every input from the start.
    let truth = ground_truth(&local, &remote);
    assert_eq!(*s.state(), truth, "rollback converged on the ground truth");
  }

  #[test]
  fn two_peers_exchanging_inputs_converge_to_the_same_world() {
    // The determinism guarantee end to end: two independent sessions, each local
    // to one player, each predicting the other, each rolling back. With every
    // input eventually delivered they must agree, and agree with ground truth.
    let p0: Vec<In> = (0..40).map(|f| In(((f * 7) % 5) as i64 - 2)).collect();
    let p1: Vec<In> = (0..40).map(|f| In(((f * 3) % 4) as i64 - 1)).collect();

    let mut a = session(); // peer A: local player 0
    let mut b = session(); // peer B: local player 1

    // A two-frame delay each way, delivered in order.
    let delay = 2u64;
    for f in 0..40u64 {
      a.queue_local_input(0, p0[f as usize]);
      b.queue_local_input(1, p1[f as usize]);

      if f >= delay {
        let past = f - delay;
        a.confirm_remote_input(1, past, p1[past as usize]); // B's input reaches A
        b.confirm_remote_input(0, past, p0[past as usize]); // A's input reaches B
      }
      a.advance_frame();
      b.advance_frame();
    }
    // Flush the inputs still in flight when the loop ended.
    for f in 38..40u64 {
      a.confirm_remote_input(1, f, p1[f as usize]);
      b.confirm_remote_input(0, f, p0[f as usize]);
    }
    a.resolve_rollback();
    b.resolve_rollback();

    let truth = ground_truth(&p0, &p1);
    assert_eq!(*a.state(), truth, "peer A converged");
    assert_eq!(*b.state(), truth, "peer B converged");
    assert_eq!(a.state(), b.state(), "the two peers agree frame for frame");
  }

  #[test]
  fn delay_based_policy_can_tell_when_a_frame_is_fully_known() {
    let mut s = session();
    s.queue_local_input(0, In(1));
    assert!(!s.is_frame_confirmed(0), "the remote input for frame 0 is not in yet");
    s.confirm_remote_input(1, 0, In(1));
    assert!(s.is_frame_confirmed(0), "now every player's frame-0 input is known");
  }

  #[test]
  fn a_correction_older_than_the_history_does_not_panic() {
    // Roll the window forward well past a frame, then confirm a contradicting
    // input for that long-evicted frame. It cannot roll back that far; it must
    // clamp to the oldest retained frame rather than panic or corrupt state.
    let mut s = RollbackSession::new(World { pos: [0, 0] }, vec![NEUTRAL, NEUTRAL], RollbackConfig { max_rollback_frames: 4 }, step);
    for f in 0..20u64 {
      s.queue_local_input(0, In(1));
      s.confirm_remote_input(1, f, In(1)); // predicted correctly along the way
      s.advance_frame();
    }
    // A late, contradicting confirmation for frame 0, long since evicted.
    s.confirm_remote_input(1, 0, In(99));
    s.advance_frame(); // must not panic
    assert!(s.state().pos[0].is_positive() || s.state().pos[0] == 0);
    let _ = s.state();
  }

  #[test]
  fn rollback_disabled_keeps_a_wrong_guess_and_diverges_from_the_truth() {
    // The same direction change as the rollback test, but with rollback off: the
    // misprediction is detected and then ignored, so the present never lands on
    // the ground truth. This is the "why rollback" contrast.
    let remote: Vec<In> = (0..10).map(|f| if f < 4 { In(1) } else { In(-3) }).collect();
    let local = vec![In(1); 10];

    let mut s = session();
    s.set_rollback_enabled(false);
    for f in 0..10u64 {
      s.queue_local_input(0, local[f as usize]);
      if f >= 2 {
        let past = f - 2;
        s.confirm_remote_input(1, past, remote[past as usize]);
      }
      s.advance_frame();
    }
    for f in 8..10u64 {
      s.confirm_remote_input(1, f, remote[f as usize]);
    }
    s.resolve_rollback();

    assert_eq!(s.rollback_count(), 0, "rollback disabled never re-simulates");
    let truth = ground_truth(&local, &remote);
    assert_ne!(*s.state(), truth, "without rollback the trusted guess never converges");
  }

  #[test]
  fn state_at_a_confirmed_frame_matches_across_two_peers() {
    // Two peers, predictions and rollbacks along the way. At a frame both have
    // fully confirmed, their saved states are identical, that is the in-sync check.
    let p0: Vec<In> = (0..30).map(|f| In(((f * 5) % 3) as i64 - 1)).collect();
    let p1: Vec<In> = (0..30).map(|f| In(((f * 2) % 3) as i64 - 1)).collect();

    let mut a = session();
    let mut b = session();
    for f in 0..30u64 {
      a.queue_local_input(0, p0[f as usize]);
      b.queue_local_input(1, p1[f as usize]);
      if f >= 3 {
        a.confirm_remote_input(1, f - 3, p1[(f - 3) as usize]);
        b.confirm_remote_input(0, f - 3, p0[(f - 3) as usize]);
      }
      a.advance_frame();
      b.advance_frame();
    }
    // A frame both peers have every input for (confirmed through f-1 at f=30 loop
    // end, minus the 3-frame delay still in flight): pick a safely-confirmed one.
    let cf = 20;
    let sa = a.state_at(cf).expect("A retains the frame");
    let sb = b.state_at(cf).expect("B retains the frame");
    assert_eq!(sa, sb, "a fully-confirmed frame is identical on both peers");
  }

  #[test]
  fn the_prediction_horizon_reflects_how_far_ahead_of_confirmation_it_is() {
    let mut s = session();
    for _ in 0..6u64 {
      s.queue_local_input(0, In(1));
      s.advance_frame();
    }
    // Local player is always confirmed; the remote has confirmed nothing, so the
    // horizon spans every simulated frame.
    assert_eq!(s.prediction_horizon(), 6);
    s.confirm_remote_input(1, 4, In(0));
    assert_eq!(s.prediction_horizon(), 1, "confirmed through frame 4, one frame ahead");
  }
}
