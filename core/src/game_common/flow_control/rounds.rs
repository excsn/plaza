//! Traits and operation payloads for managing games structured into multiple rounds.

use crate::agent::AgentId;
use crate::common::fsm::FsmContext;
use crate::session::TargetedOp;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::marker::PhantomData;

/// Trait for managing game rounds.
///
/// - `Op`: The application's operation type.
/// - `AppID`: The application's `AgentId` type.
pub trait RoundManager<Op, AppID: AgentId> {
  /// Gets the current round number (e.g., 1-indexed).
  fn current_round(&self) -> u32;

  /// Gets the maximum number of rounds for this match, if defined.
  fn max_rounds(&self) -> Option<u32>;

  /// Attempts to start the next round.
  /// Returns `Ok(())` or `Err(ReasonString)` (e.g., if max rounds reached).
  /// This method is expected to enqueue a `RoundStartedNoticeOp` (via op_payloads)
  /// into the context's operation queue.
  fn start_next_round(&mut self, context: &mut dyn FsmContext<Op, AppID>) -> Result<(), String>;

  /// Ends the current round explicitly.
  /// This method is expected to enqueue a `RoundEndedNoticeOp` (via op_payloads).
  fn end_current_round(
    &mut self,
    context: &mut dyn FsmContext<Op, AppID>,
    reason: String, /*, round_winner_data: Option<AppSpecificWinnerData> */
  );
}

/// Counts rounds up to an optional maximum.
///
/// One implementation of [`RoundManager`], not the only one: swap in your own
/// for best-of-N, sudden death, or elimination brackets.
///
/// As with turns, you supply the constructors wrapping the notice payloads into
/// your `Op` type. `Summary` is whatever your game reports at the end of a round
/// (scores, a winner); use `()` if there is nothing to report.
///
/// ```ignore
/// let mut rounds = SequentialRoundManager::new(Some(5), MyOp::RoundStarted, MyOp::RoundEnded);
/// rounds.start_next_round(&mut ctx)?;
/// rounds.end_round_with(&mut ctx, "all players folded", Some(summary));
/// ```
///
/// Those are plain `fn` pointers rather than boxed closures, deliberately: a
/// boxed closure would cost this type `Clone`. A non-capturing closure such as
/// `|n| MyOp::RoundStarted(n)` coerces to one, so the only thing ruled out is
/// capturing state, which is what writing your own [`RoundManager`] is for.
pub struct SequentialRoundManager<Op, AppID: AgentId, Summary: Clone + Debug> {
  current_round: u32,
  max_rounds: Option<u32>,
  /// Whether a round is in progress, so ending twice is caught.
  in_progress: bool,
  started_notice: fn(op_payloads::RoundStartedNoticePayload) -> Op,
  ended_notice: fn(op_payloads::RoundEndedNoticePayload<Summary>) -> Op,
  _phantom: PhantomData<fn() -> AppID>,
}

// Hand-written rather than derived: deriving would demand `Op: Clone`, though
// `Op` appears only behind function pointers, which are always `Copy`.
impl<Op, AppID: AgentId, Summary: Clone + Debug> Clone for SequentialRoundManager<Op, AppID, Summary> {
  fn clone(&self) -> Self {
    Self {
      current_round: self.current_round,
      max_rounds: self.max_rounds,
      in_progress: self.in_progress,
      started_notice: self.started_notice,
      ended_notice: self.ended_notice,
      _phantom: PhantomData,
    }
  }
}

impl<Op, AppID: AgentId, Summary: Clone + Debug> Debug for SequentialRoundManager<Op, AppID, Summary> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("SequentialRoundManager")
      .field("current_round", &self.current_round)
      .field("max_rounds", &self.max_rounds)
      .field("in_progress", &self.in_progress)
      .finish()
  }
}

impl<Op, AppID: AgentId, Summary: Clone + Debug> SequentialRoundManager<Op, AppID, Summary> {
  /// Creates a manager. `max_rounds` of `None` means the game runs until the
  /// application decides otherwise.
  pub fn new(
    max_rounds: Option<u32>,
    started_notice: fn(op_payloads::RoundStartedNoticePayload) -> Op,
    ended_notice: fn(op_payloads::RoundEndedNoticePayload<Summary>) -> Op,
  ) -> Self {
    Self {
      current_round: 0,
      max_rounds,
      in_progress: false,
      started_notice,
      ended_notice,
      _phantom: PhantomData,
    }
  }

  /// Whether a round is currently running.
  pub fn round_in_progress(&self) -> bool {
    self.in_progress
  }

  /// Whether every round has been played.
  pub fn is_finished(&self) -> bool {
    self.max_rounds.is_some_and(|max| self.current_round >= max) && !self.in_progress
  }

  /// Ends the round with a summary: scores, a winner, whatever your game reports.
  ///
  /// [`end_current_round`](RoundManager::end_current_round) is the same thing
  /// without a summary.
  pub fn end_round_with(
    &mut self,
    context: &mut dyn FsmContext<Op, AppID>,
    reason: impl Into<String>,
    summary: Option<Summary>,
  ) {
    if !self.in_progress {
      return;
    }
    self.in_progress = false;
    let payload = op_payloads::RoundEndedNoticePayload {
      round_number: self.current_round,
      reason: reason.into(),
      summary_data: summary,
    };
    let op = (self.ended_notice)(payload);
    context.ops_q().push(TargetedOp::new_system_all(vec![op]));
  }
}

impl<Op, AppID: AgentId, Summary: Clone + Debug> RoundManager<Op, AppID> for SequentialRoundManager<Op, AppID, Summary> {
  fn current_round(&self) -> u32 {
    self.current_round
  }

  fn max_rounds(&self) -> Option<u32> {
    self.max_rounds
  }

  /// Starts the next round, emitting a notice.
  ///
  /// Fails if the round limit is reached or a round is still running: ending a
  /// round is explicit, so a game cannot silently skip scoring it.
  fn start_next_round(&mut self, context: &mut dyn FsmContext<Op, AppID>) -> Result<(), String> {
    if self.in_progress {
      return Err(format!("round {} is still in progress", self.current_round));
    }
    if let Some(max) = self.max_rounds {
      if self.current_round >= max {
        return Err(format!("all {max} rounds have been played"));
      }
    }

    self.current_round += 1;
    self.in_progress = true;
    let payload = op_payloads::RoundStartedNoticePayload {
      round_number: self.current_round,
      total_rounds: self.max_rounds,
    };
    let op = (self.started_notice)(payload);
    context.ops_q().push(TargetedOp::new_system_all(vec![op]));
    Ok(())
  }

  fn end_current_round(&mut self, context: &mut dyn FsmContext<Op, AppID>, reason: String) {
    self.end_round_with(context, reason, None);
  }
}

/// Defines common operation payloads related to game rounds.
pub mod op_payloads {
  use super::*;
                // use std::any::Any; // If AppSpecificRoundSummaryData is truly generic via dyn Any + serde attributes

  /// Payload for an Op that signals the start of a new round.
  /// Typically generated by `StateLogic` (driven by a `RoundManager`) and broadcast.
  #[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
  pub struct RoundStartedNoticePayload {
    pub round_number: u32,
    pub total_rounds: Option<u32>, // If a fixed number of rounds is known
  }

  /// Payload for an Op that signals the end of a round.
  /// `AppSpecificRoundSummaryData` would be a type defined by the application,
  /// needing `Clone + Debug + Serialize + Deserialize`.
  #[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
  #[serde(bound = "AppSpecificRoundSummaryData: Serialize + for<'de2> Deserialize<'de2>")]
    pub struct RoundEndedNoticePayload<AppSpecificRoundSummaryData: Clone + Debug> {
    pub round_number: u32,
    pub reason: String,
    pub summary_data: Option<AppSpecificRoundSummaryData>, // e.g., round winner, scores this round
  }
}

#[cfg(test)]
mod tests {
  use super::op_payloads::{RoundEndedNoticePayload, RoundStartedNoticePayload};
  use super::*;
  use crate::common::fsm::OpsQueue;

  #[derive(Debug, Clone, PartialEq)]
  struct Summary {
    winner: u8,
  }

  #[derive(Debug, Clone, PartialEq)]
  enum TestOp {
    Started(RoundStartedNoticePayload),
    Ended(RoundEndedNoticePayload<Summary>),
  }

  type Rounds = SequentialRoundManager<TestOp, u64, Summary>;
  type Ctx = OpsQueue<TestOp, u64>;

  fn manager(max: Option<u32>) -> Rounds {
    SequentialRoundManager::new(max, TestOp::Started, TestOp::Ended)
  }

  fn ops(ctx: Ctx) -> Vec<TestOp> {
    ctx.into_ops().into_iter().flat_map(|t| t.ops).collect()
  }

  #[test]
  fn rounds_count_up_from_zero() {
    let mut rounds = manager(Some(3));
    let mut ctx = Ctx::new();
    assert_eq!(rounds.current_round(), 0, "no round before the first start");

    rounds.start_next_round(&mut ctx).unwrap();
    assert_eq!(rounds.current_round(), 1);
    assert!(rounds.round_in_progress());
  }

  #[test]
  fn a_round_must_end_before_the_next_starts() {
    let mut rounds = manager(None);
    let mut ctx = Ctx::new();
    rounds.start_next_round(&mut ctx).unwrap();

    assert!(
      rounds.start_next_round(&mut ctx).is_err(),
      "starting twice would skip scoring the first round"
    );

    rounds.end_current_round(&mut ctx, "done".into());
    assert!(rounds.start_next_round(&mut ctx).is_ok());
    assert_eq!(rounds.current_round(), 2);
  }

  #[test]
  fn the_round_limit_is_enforced() {
    let mut rounds = manager(Some(2));
    let mut ctx = Ctx::new();

    for _ in 0..2 {
      rounds.start_next_round(&mut ctx).unwrap();
      rounds.end_current_round(&mut ctx, "done".into());
    }
    assert!(rounds.is_finished());
    assert!(rounds.start_next_round(&mut ctx).is_err());
  }

  #[test]
  fn an_unlimited_game_never_reports_finished() {
    let mut rounds = manager(None);
    let mut ctx = Ctx::new();
    for _ in 0..20 {
      rounds.start_next_round(&mut ctx).unwrap();
      rounds.end_current_round(&mut ctx, "done".into());
    }
    assert!(!rounds.is_finished());
    assert_eq!(rounds.current_round(), 20);
  }

  #[test]
  fn notices_carry_the_round_number_and_total() {
    let mut rounds = manager(Some(5));
    let mut ctx = Ctx::new();
    rounds.start_next_round(&mut ctx).unwrap();
    rounds.end_round_with(&mut ctx, "all folded", Some(Summary { winner: 7 }));

    match &ops(ctx)[..] {
      [TestOp::Started(started), TestOp::Ended(ended)] => {
        assert_eq!(started.round_number, 1);
        assert_eq!(started.total_rounds, Some(5));
        assert_eq!(ended.round_number, 1);
        assert_eq!(ended.reason, "all folded");
        assert_eq!(ended.summary_data, Some(Summary { winner: 7 }));
      }
      other => panic!("expected a start then an end, got {:?}", other),
    }
  }

  #[test]
  fn a_clone_advances_independently_of_the_original() {
    // A game that searches ahead clones its state and re-runs rounds in
    // simulation. This is why the notice constructors are `fn` pointers and not
    // boxed closures: the boxed version was not `Clone`, so this was impossible
    // to write at all.
    let mut live = manager(Some(5));
    let mut ctx = Ctx::new();
    live.start_next_round(&mut ctx).unwrap();

    let mut sim = live.clone();
    sim.end_current_round(&mut ctx, "simulated".into());
    sim.start_next_round(&mut ctx).unwrap();

    assert_eq!(sim.current_round(), 2, "the simulation ran ahead");
    assert_eq!(live.current_round(), 1, "the real game did not move");
    assert!(live.round_in_progress(), "and its round is still open");
  }

  #[test]
  fn ending_a_round_that_is_not_running_does_nothing() {
    let mut rounds = manager(None);
    let mut ctx = Ctx::new();
    rounds.end_current_round(&mut ctx, "spurious".into());
    assert!(ctx.is_empty(), "no notice for a round that never started");
  }
}
