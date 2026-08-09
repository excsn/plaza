//! What phase play is in: [`Phased`] to hold it, payloads to announce it.
//!
//! Two questions get confused here, and separating them is the whole design.
//!
//! **Which phases exist, when they change, and what is legal is yours.** This
//! module ships no controller, because every shape for one fails against real
//! games:
//!
//! - The guard that matters is usually compound, "this phase *and* this
//!   player's turn", so a controller that does not know turn order cannot
//!   express it.
//! - Some phases are not resting states. A resolution step may be entered and
//!   left within a single call, broadcast purely so clients can animate it, so
//!   modelling "the current phase" as something observable is wrong.
//! - End conditions tend to be polled at every mutation point, with different
//!   early-return behaviour at each, rather than guarding one edge.
//! - Games that search ahead clone their state and re-run transitions in
//!   simulation, so phase logic has to stay pure, synchronous, and cheaply
//!   clonable. A controller owning timers or channels breaks that outright,
//!   which is also why [`StateMachine`](crate::common::fsm::StateMachine) is
//!   not a drop-in answer here: it holds boxed trait objects and is not `Clone`.
//!
//! **That the change reaches clients is not yours, it is arithmetic.** A phase
//! that moves without a notice going out is a client whose view has silently
//! diverged, and the reliable fix is to make the field unreachable except
//! through something that does both. That is [`Phased`], and it is the same
//! invariant [`turns`](super::turns) and [`rounds`](super::rounds) enforce for
//! their own state. It decides nothing: no transition table, no legality rules,
//! no timers, no knowledge of turn order.

use crate::agent::AgentId;
use crate::common::fsm::FsmContext;
use crate::session::TargetedOp;
use std::fmt::Debug;
use std::time::Duration;

use op_payloads::PhaseChangedNoticePayload;

/// A token identifying one occupancy of a phase.
///
/// Work scheduled inside a phase often resumes after the world has moved: a
/// think-delay finishes, a timeout fires, a task wakes. Re-deriving "is this
/// still relevant" from whatever fields are in scope is the pattern that gets
/// written slightly differently at every call site, and wrong at one of them.
///
/// Capture an `Epoch` when scheduling, compare it on resume. Because every
/// transition goes through [`Phased`], the counter cannot fall behind.
///
/// ```ignore
/// // Scheduling: remember which occupancy this belongs to.
/// let token = state.phase.epoch();
/// state.scheduler.schedule_after(state.now, THINK_DELAY, PendingMove { player, token });
///
/// // Resuming, possibly several transitions later.
/// for due in state.scheduler.tick(state.now) {
///   if !state.phase.is_current(due.token) {
///     continue;
///   }
///   state.apply_move(due.player, &mut ctx);
/// }
/// ```
///
/// A stale token means only "the phase has changed since". What that implies,
/// dropping the work, re-queueing it, or treating it as a forfeit, is
/// application policy and plaza does not decide it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Epoch(u64);

/// The current phase, and the guarantee that clients hear about every change.
///
/// Holds a phase and an [`Epoch`]. It knows nothing about turn order, timers,
/// or transition rules, and takes no position on any of them; see the [module
/// docs](self) for where that line falls and why.
///
/// `Clone` whenever `P` is, deliberately: games that search ahead clone their
/// whole state and re-run transitions in simulation, so nothing here stores a
/// closure, a timer, or a channel. The notice constructor is a plain function
/// pointer passed per call for the same reason.
///
/// # Holding one
///
/// ```ignore
/// #[derive(Clone, Debug, PartialEq)]
/// enum GamePhase { Setup, PlayerTurn, Resolving, Finished }
///
/// #[derive(Clone)]
/// struct Game {
///   phase: Phased<GamePhase>,
///   current_player: PlayerId,
/// }
///
/// let game = Game {
///   phase: Phased::new(GamePhase::Setup),
///   current_player: alice,
/// };
/// ```
///
/// # Changing it
///
/// As with [`turns`](super::turns) and [`rounds`](super::rounds), you supply the
/// constructor that wraps the notice payload into your `Op`, because plaza
/// cannot know your enum:
///
/// ```ignore
/// async fn process_input(&self, state: &mut Game, input: LogicInput<GameOp, PlayerId>)
///   -> Result<LogicOutput<GameOp, PlayerId>, StateLogicError>
/// {
///   let mut ctx = OpsQueue::new();
///
///   if let LogicInput::AgentOps { source, ops } = input {
///     for op in ops {
///       // The guard stays yours: it is compound, mixing phase with whose turn
///       // it is. `Phased` never sees it.
///       match (state.phase.current(), &op) {
///         (GamePhase::PlayerTurn, GameOp::PlayCard(card))
///           if Some(&state.current_player) == source.id() =>
///         {
///           state.play(card, &mut ctx);
///
///           // One call: assigns, bumps the epoch, emits the notice.
///           // Forgetting the broadcast is not expressible.
///           state.phase.transition_to(GamePhase::Resolving, &mut ctx, GameOp::PhaseChanged);
///         }
///         _ => return Err(StateLogicError::Rejected("not your turn".into())),
///       }
///     }
///   }
///
///   Ok(ctx.into_ops().into())
/// }
/// ```
///
/// A phase entered and left inside one call is two transitions and two notices,
/// which is what a client animating the step wants anyway:
///
/// ```ignore
/// state.phase.transition_to(GamePhase::Resolving, &mut ctx, GameOp::PhaseChanged);
/// let outcome = state.resolve_board();
/// state.phase.transition_to(GamePhase::PlayerTurn, &mut ctx, GameOp::PhaseChanged);
/// ```
///
/// # Simulating ahead
///
/// Nothing here allocates, locks, or holds a callback, so a search can clone the
/// whole state and drive transitions at the same cost as live play:
///
/// ```ignore
/// fn rollout(&self, from: &Game) -> Outcome {
///   let mut sim = from.clone();
///   let mut sink = OpsQueue::new();   // notices are generated and discarded
///
///   while *sim.phase.current() != GamePhase::Finished && sim.turns < MAX_ROLLOUT {
///     let mv = self.policy.pick(&sim);
///     sim.apply(mv, &mut sink);
///   }
///   sim.outcome()
/// }
/// ```
///
/// The same transition code runs in both paths, so simulation cannot drift from
/// live play.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Phased<P> {
  current: P,
  epoch: u64,
}

impl<P> Phased<P> {
  /// Starts in `initial`, announcing nothing. The opening phase is part of the
  /// state a joining client is snapshotted with, not a change to broadcast.
  pub fn new(initial: P) -> Self {
    Self {
      current: initial,
      epoch: 0,
    }
  }

  /// The phase now in effect.
  pub fn current(&self) -> &P {
    &self.current
  }

  /// A token for the current occupancy, to hand to work that will resume later.
  pub fn epoch(&self) -> Epoch {
    Epoch(self.epoch)
  }

  /// Whether `epoch` still refers to the occupancy in effect.
  ///
  /// `false` means the phase has changed since the token was taken. What to do
  /// about that is yours.
  pub fn is_current(&self, epoch: Epoch) -> bool {
    self.epoch == epoch.0
  }
}

impl<P: Clone + Debug + PartialEq> Phased<P> {
  /// Moves to `next`, bumping the epoch and emitting a notice, or does nothing.
  ///
  /// Returns whether the phase actually changed. Transitioning to the phase
  /// already in effect is a no-op that emits no notice and leaves the epoch
  /// alone, so an end-condition check that runs at several call sites cannot
  /// spam clients with duplicates.
  pub fn transition_to<Op, AppID: AgentId>(
    &mut self,
    next: P,
    context: &mut dyn FsmContext<Op, AppID>,
    notice: fn(PhaseChangedNoticePayload<P>) -> Op,
  ) -> bool {
    self.transition_with(next, context, notice, None, None)
  }

  /// [`transition_to`](Self::transition_to) with the rest of the notice filled
  /// in: why it changed, and how long clients should expect the new phase to
  /// last.
  ///
  /// ```ignore
  /// state.phase.transition_with(
  ///   GamePhase::Finished, &mut ctx, GameOp::PhaseChanged,
  ///   Some("time limit reached".into()), None,
  /// );
  ///
  /// state.phase.transition_with(
  ///   GamePhase::PlayerTurn, &mut ctx, GameOp::PhaseChanged,
  ///   None, Some(Duration::from_secs(30)),   // clients can render a countdown
  /// );
  /// ```
  ///
  /// A `duration_hint` is a hint. Enforcing it is the application's job, the
  /// same division [`RoundRobinTurnManager::with_time_limit`](super::turns::RoundRobinTurnManager::with_time_limit)
  /// draws: pair it with a scheduler.
  pub fn transition_with<Op, AppID: AgentId>(
    &mut self,
    next: P,
    context: &mut dyn FsmContext<Op, AppID>,
    notice: fn(PhaseChangedNoticePayload<P>) -> Op,
    reason: Option<String>,
    duration_hint: Option<Duration>,
  ) -> bool {
    if self.current == next {
      return false;
    }

    let previous = std::mem::replace(&mut self.current, next);
    self.epoch += 1;

    let payload = PhaseChangedNoticePayload {
      new_phase: self.current.clone(),
      previous_phase: Some(previous),
      duration_hint,
      reason,
    };
    context.ops_q().push(TargetedOp::new_system_all(vec![notice(payload)]));
    true
  }
}

/// Defines common operation payloads related to game phases.
pub mod op_payloads {
  pub use plaza_wire::flow_payloads::{CountdownTickNoticePayload, PhaseChangedNoticePayload, RequestPhaseTransitionPayload};
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::common::fsm::OpsQueue;

  #[derive(Debug, Clone, PartialEq)]
  enum Phase {
    Setup,
    Playing,
    Resolving,
    Finished,
  }

  #[derive(Debug, Clone, PartialEq)]
  enum TestOp {
    PhaseChanged(PhaseChangedNoticePayload<Phase>),
  }

  type Ctx = OpsQueue<TestOp, u64>;

  fn notices(ctx: Ctx) -> Vec<PhaseChangedNoticePayload<Phase>> {
    ctx
      .into_ops()
      .into_iter()
      .flat_map(|t| t.ops)
      .map(|TestOp::PhaseChanged(n)| n)
      .collect()
  }

  #[test]
  fn the_opening_phase_announces_nothing() {
    let phase = Phased::new(Phase::Setup);
    assert_eq!(phase.current(), &Phase::Setup);
  }

  #[test]
  fn a_transition_announces_both_sides() {
    let mut phase = Phased::new(Phase::Setup);
    let mut ctx = Ctx::new();

    assert!(phase.transition_to(Phase::Playing, &mut ctx, TestOp::PhaseChanged));
    assert_eq!(phase.current(), &Phase::Playing);

    match &notices(ctx)[..] {
      [notice] => {
        assert_eq!(notice.new_phase, Phase::Playing);
        assert_eq!(notice.previous_phase, Some(Phase::Setup));
      }
      other => panic!("expected exactly one notice, got {other:?}"),
    }
  }

  #[test]
  fn changing_the_phase_without_announcing_it_is_not_expressible() {
    // The point of the type: every path that moves the phase emits a notice,
    // so a client cannot silently diverge from the server.
    let mut phase = Phased::new(Phase::Setup);
    let mut ctx = Ctx::new();

    phase.transition_to(Phase::Playing, &mut ctx, TestOp::PhaseChanged);
    phase.transition_to(Phase::Resolving, &mut ctx, TestOp::PhaseChanged);
    phase.transition_to(Phase::Finished, &mut ctx, TestOp::PhaseChanged);

    assert_eq!(notices(ctx).len(), 3, "one notice per change, no more, no fewer");
  }

  #[test]
  fn transitioning_to_the_current_phase_does_nothing() {
    // An end-condition check often runs at several call sites in one update;
    // it must not spam clients with duplicate notices.
    let mut phase = Phased::new(Phase::Finished);
    let mut ctx = Ctx::new();

    assert!(!phase.transition_to(Phase::Finished, &mut ctx, TestOp::PhaseChanged));
    assert!(ctx.is_empty(), "no notice for a phase that did not change");
  }

  #[test]
  fn a_no_op_transition_leaves_scheduled_work_valid() {
    let mut phase = Phased::new(Phase::Playing);
    let mut ctx = Ctx::new();
    let token = phase.epoch();

    phase.transition_to(Phase::Playing, &mut ctx, TestOp::PhaseChanged);
    assert!(phase.is_current(token), "nothing changed, so nothing went stale");
  }

  #[test]
  fn work_scheduled_before_a_transition_goes_stale() {
    let mut phase = Phased::new(Phase::Playing);
    let mut ctx = Ctx::new();
    let token = phase.epoch();
    assert!(phase.is_current(token));

    phase.transition_to(Phase::Resolving, &mut ctx, TestOp::PhaseChanged);
    assert!(!phase.is_current(token), "the world moved underneath it");

    assert!(phase.is_current(phase.epoch()), "a fresh token is current");
  }

  #[test]
  fn returning_to_an_earlier_phase_does_not_revive_stale_work() {
    // Epochs count occupancies, not phases: a token from the first Playing
    // must not validate against the second.
    let mut phase = Phased::new(Phase::Playing);
    let mut ctx = Ctx::new();
    let first = phase.epoch();

    phase.transition_to(Phase::Resolving, &mut ctx, TestOp::PhaseChanged);
    phase.transition_to(Phase::Playing, &mut ctx, TestOp::PhaseChanged);

    assert_eq!(phase.current(), &Phase::Playing, "back where we started");
    assert!(!phase.is_current(first), "but a different occupancy of it");
  }

  #[test]
  fn a_reason_and_a_countdown_ride_along_on_the_notice() {
    let mut phase = Phased::new(Phase::Playing);
    let mut ctx = Ctx::new();

    phase.transition_with(
      Phase::Finished,
      &mut ctx,
      TestOp::PhaseChanged,
      Some("time limit reached".into()),
      Some(Duration::from_secs(30)),
    );

    let notice = &notices(ctx)[0];
    assert_eq!(notice.reason.as_deref(), Some("time limit reached"));
    assert_eq!(notice.duration_hint, Some(Duration::from_secs(30)));
  }

  #[test]
  fn a_phase_entered_and_left_in_one_call_announces_twice() {
    // A resolution step that is never a resting state still has to be visible
    // to a client animating it.
    let mut phase = Phased::new(Phase::Playing);
    let mut ctx = Ctx::new();

    phase.transition_to(Phase::Resolving, &mut ctx, TestOp::PhaseChanged);
    phase.transition_to(Phase::Playing, &mut ctx, TestOp::PhaseChanged);

    let notices = notices(ctx);
    assert_eq!(notices.len(), 2);
    assert_eq!(notices[0].new_phase, Phase::Resolving);
    assert_eq!(notices[1].new_phase, Phase::Playing);
  }

  #[test]
  fn a_clone_advances_independently_of_the_original() {
    // Games that search ahead clone their whole state and re-run transitions in
    // simulation. Nothing here may be shared between the two.
    let mut live = Phased::new(Phase::Playing);
    let mut ctx = Ctx::new();

    let mut sim = live.clone();
    sim.transition_to(Phase::Finished, &mut ctx, TestOp::PhaseChanged);

    assert_eq!(sim.current(), &Phase::Finished);
    assert_eq!(live.current(), &Phase::Playing, "the real game did not move");

    let token = live.epoch();
    live.transition_to(Phase::Resolving, &mut ctx, TestOp::PhaseChanged);
    assert!(!live.is_current(token));
    assert!(sim.is_current(sim.epoch()), "the simulation kept its own clock");
  }
}
