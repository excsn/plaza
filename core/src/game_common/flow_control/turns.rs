//! Traits and operation payloads for managing turns in turn-based games.

use crate::agent::AgentId;
use crate::common::fsm::FsmContext;
use crate::session::TargetedOp;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::marker::PhantomData;
use std::time::Duration;

/// Trait for managing discrete turns for players or teams.
///
/// - `Op`: The application's operation type.
/// - `AppID`: The application's `AgentId` type.
/// - `TurnActorId`: Application-defined type representing whose turn it is (e.g., PlayerId, TeamId).
pub trait TurnManager<Op, AppID: AgentId, TurnActorId> {
  /// Gets the ID of the entity whose turn it currently is.
  fn current_turn_actor(&self) -> Option<TurnActorId>;

  /// Attempts to end the current turn and advance to the next.
  /// Implementation determines turn order (e.g., round-robin, custom logic).
  /// Returns `Ok(Option<NextTurnActorId>)` where `None` might indicate end of round/all turns.
  /// This method is expected to enqueue a `TurnChangedNoticeOp` (via op_payloads)
  /// into the context's operation queue upon successful advancement.
  fn end_current_turn_and_advance(
    &mut self,
    context: &mut dyn FsmContext<Op, AppID>,
  ) -> Result<Option<TurnActorId>, String>;

}

/// Round-robin [`TurnManager`] over an ordered roster.
///
/// One implementation of the trait, not the only one: write your own for
/// initiative order, bidding, or anything else, and the rest of `flow_control`
/// still applies.
///
/// Because a manager cannot know your `Op` type, you supply the constructor that
/// wraps a [`TurnChangedNoticePayload`](op_payloads::TurnChangedNoticePayload)
/// into it:
///
/// ```ignore
/// let mut turns = RoundRobinTurnManager::new(vec![alice, bob, carol], MyOp::TurnChanged);
/// turns.begin(&mut ctx);                        // seat the first actor
/// turns.end_current_turn_and_advance(&mut ctx)?; // hand off, emitting a notice
/// ```
///
/// That is a plain `fn` pointer rather than a boxed closure, deliberately: a
/// boxed closure would cost this type `Clone`. A non-capturing closure such as
/// `|n| MyOp::TurnChanged(n)` coerces to one, so the only thing ruled out is
/// capturing state, which is what writing your own [`TurnManager`] is for.
pub struct RoundRobinTurnManager<Op, AppID: AgentId, TurnActorId: Clone + Debug> {
  actors: Vec<TurnActorId>,
  /// Index into `actors`; `None` before the first turn begins.
  current: Option<usize>,
  turn_number: u32,
  time_limit: Option<Duration>,
  notice: fn(op_payloads::TurnChangedNoticePayload<TurnActorId>) -> Op,
  _phantom: PhantomData<fn() -> AppID>,
}

// Hand-written rather than derived: deriving would demand `Op: Clone`, though
// `Op` appears only behind a function pointer, which is always `Copy`.
impl<Op, AppID, TurnActorId> Clone for RoundRobinTurnManager<Op, AppID, TurnActorId>
where
  AppID: AgentId,
  TurnActorId: Clone + Debug,
{
  fn clone(&self) -> Self {
    Self {
      actors: self.actors.clone(),
      current: self.current,
      turn_number: self.turn_number,
      time_limit: self.time_limit,
      notice: self.notice,
      _phantom: PhantomData,
    }
  }
}

impl<Op, AppID, TurnActorId> Debug for RoundRobinTurnManager<Op, AppID, TurnActorId>
where
  AppID: AgentId,
  TurnActorId: Clone + Debug,
{
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("RoundRobinTurnManager")
      .field("actors", &self.actors)
      .field("current", &self.current)
      .field("turn_number", &self.turn_number)
      .finish()
  }
}

impl<Op, AppID, TurnActorId> RoundRobinTurnManager<Op, AppID, TurnActorId>
where
  AppID: AgentId,
  TurnActorId: Clone + Debug + PartialEq,
{
  /// Creates a manager over `actors`, in the order they will play.
  ///
  /// No turn is active until [`begin`](Self::begin) is called.
  pub fn new(actors: Vec<TurnActorId>, notice: fn(op_payloads::TurnChangedNoticePayload<TurnActorId>) -> Op) -> Self {
    Self {
      actors,
      current: None,
      turn_number: 0,
      time_limit: None,
      notice,
      _phantom: PhantomData,
    }
  }

  /// Announces a per-turn time limit on every notice. Enforcing it is the
  /// application's job: pair it with a scheduler.
  pub fn with_time_limit(mut self, limit: Duration) -> Self {
    self.time_limit = Some(limit);
    self
  }

  /// Starts the first turn, emitting a notice. No-op if the roster is empty or
  /// a turn is already active.
  pub fn begin(&mut self, context: &mut dyn FsmContext<Op, AppID>) -> Option<TurnActorId> {
    if self.current.is_some() || self.actors.is_empty() {
      return self.current_turn_actor();
    }
    self.current = Some(0);
    self.turn_number = 1;
    self.emit_notice(context, None);
    self.current_turn_actor()
  }

  /// Seats the first actor again and starts the count over, emitting a notice.
  ///
  /// For a game whose order restarts, most often at the top of each round.
  /// [`begin`](Self::begin) will not do this: it returns early while a turn is
  /// active, and after the last actor of a round plays there still is one,
  /// because the turn only advances when someone is left to play.
  ///
  /// ```ignore
  /// rounds.start_next_round(&mut ctx)?;
  /// turns.restart(&mut ctx);   // back to the first seat, turn 1
  /// ```
  ///
  /// [`turn_number`](Self::turn_number) resets to 1, because it counts turns
  /// since the order began and this begins it again. A game that instead wants
  /// one continuous count across rounds does not want `restart` at all: keep a
  /// single manager and keep advancing, letting the roster wrap.
  ///
  /// Safe to call before the first [`begin`](Self::begin), where it does the
  /// same thing. Does nothing on an empty roster.
  pub fn restart(&mut self, context: &mut dyn FsmContext<Op, AppID>) -> Option<TurnActorId> {
    if self.actors.is_empty() {
      self.current = None;
      return None;
    }

    let previous = self.current_turn_actor();
    self.current = Some(0);
    self.turn_number = 1;
    self.emit_notice(context, previous);
    self.current_turn_actor()
  }

  /// Adds an actor to the end of the order. Does not disturb the current turn.
  pub fn add_actor(&mut self, actor: TurnActorId) {
    self.actors.push(actor);
  }

  /// Removes an actor, e.g. a player who disconnected.
  ///
  /// Removing the actor whose turn it is leaves the turn pointing at whoever now
  /// occupies that slot, so play continues rather than stalling on someone who
  /// left. Returns whether the actor was present.
  pub fn remove_actor(&mut self, actor: &TurnActorId) -> bool {
    let Some(index) = self.actors.iter().position(|a| a == actor) else {
      return false;
    };
    self.actors.remove(index);

    match self.current {
      _ if self.actors.is_empty() => self.current = None,
      Some(current) if index < current => self.current = Some(current - 1),
      // The removed actor held the turn: the slot now holds the next player,
      // except at the end of the order, where it wraps.
      Some(current) if index == current && current >= self.actors.len() => self.current = Some(0),
      _ => {}
    }
    true
  }

  pub fn actors(&self) -> &[TurnActorId] {
    &self.actors
  }

  /// Turns taken since [`begin`](Self::begin), counting from 1, or 0 before it.
  pub fn turn_number(&self) -> u32 {
    self.turn_number
  }

  fn emit_notice(&self, context: &mut dyn FsmContext<Op, AppID>, previous: Option<TurnActorId>) {
    let payload = op_payloads::TurnChangedNoticePayload {
      new_turn_actor: self.current_turn_actor(),
      previous_turn_actor: previous,
      turn_number: self.turn_number,
      time_limit_for_turn: self.time_limit,
    };
    let op = (self.notice)(payload);
    context.ops_q().push(TargetedOp::new_system_all(vec![op]));
  }
}

impl<Op, AppID, TurnActorId> TurnManager<Op, AppID, TurnActorId> for RoundRobinTurnManager<Op, AppID, TurnActorId>
where
  AppID: AgentId,
  TurnActorId: Clone + Debug + PartialEq,
{
  fn current_turn_actor(&self) -> Option<TurnActorId> {
    self.current.and_then(|i| self.actors.get(i).cloned())
  }

  /// Advances to the next actor, wrapping at the end of the order.
  ///
  /// Returns the new actor, or `Err` if no turn has begun or the roster is empty.
  fn end_current_turn_and_advance(
    &mut self,
    context: &mut dyn FsmContext<Op, AppID>,
  ) -> Result<Option<TurnActorId>, String> {
    if self.actors.is_empty() {
      return Err("no actors in the turn order".to_string());
    }
    let Some(current) = self.current else {
      return Err("no turn is active; call begin() first".to_string());
    };

    let previous = self.actors.get(current).cloned();
    self.current = Some((current + 1) % self.actors.len());
    self.turn_number = self.turn_number.saturating_add(1);
    self.emit_notice(context, previous);
    Ok(self.current_turn_actor())
  }
}

/// Defines common operation payloads related to game turns.
pub mod op_payloads {
  use super::*;

  /// Payload for an Op sent by a client to indicate they are ending their turn.
  #[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
  #[serde(bound = "")]
  pub struct EndTurnRequestPayload<AppID: AgentId> {
    pub player_id: AppID, // The player making the request (for validation)
  }

  /// Payload for an Op that signals a change in whose turn it is.
  /// Typically generated by `StateLogic` (driven by a `TurnManager`) and broadcast to clients.
  #[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
  #[serde(bound = "TurnActorId: Serialize + for<'de2> Deserialize<'de2>")]
  pub struct TurnChangedNoticePayload<TurnActorId: Clone + Debug> {
    pub new_turn_actor: Option<TurnActorId>, // None if, e.g., turns are over for a round
    pub previous_turn_actor: Option<TurnActorId>,
    /// Turns taken since the order began, counting from 1.
    ///
    /// Whether that means "this round" or "this match" is decided by how long
    /// you keep one manager: a per-round manager counts per round, a match-long
    /// one counts the match.
    pub turn_number: u32,
    pub time_limit_for_turn: Option<Duration>,
  }
}

#[cfg(test)]
mod tests {
  use super::op_payloads::TurnChangedNoticePayload;
  use super::*;
  use crate::common::fsm::OpsQueue;

  type PlayerId = u8;

  #[derive(Debug, Clone, PartialEq)]
  enum TestOp {
    TurnChanged(TurnChangedNoticePayload<PlayerId>),
  }

  type Turns = RoundRobinTurnManager<TestOp, u64, PlayerId>;
  type Ctx = OpsQueue<TestOp, u64>;

  fn manager(actors: Vec<PlayerId>) -> Turns {
    RoundRobinTurnManager::new(actors, TestOp::TurnChanged)
  }

  /// The notice each emitted op carries.
  fn notices(ctx: Ctx) -> Vec<TurnChangedNoticePayload<PlayerId>> {
    ctx
      .into_ops()
      .into_iter()
      .flat_map(|t| t.ops)
      .map(|TestOp::TurnChanged(n)| n)
      .collect()
  }

  #[test]
  fn no_turn_is_active_until_begin() {
    let mut turns = manager(vec![1, 2, 3]);
    assert_eq!(turns.current_turn_actor(), None);

    let mut ctx = Ctx::new();
    assert_eq!(turns.begin(&mut ctx), Some(1));
    assert_eq!(turns.current_turn_actor(), Some(1));
    assert_eq!(notices(ctx).len(), 1, "beginning announces the first turn");
  }

  #[test]
  fn advancing_wraps_around_the_order() {
    let mut turns = manager(vec![1, 2, 3]);
    let mut ctx = Ctx::new();
    turns.begin(&mut ctx);

    for expected in [2, 3, 1] {
      assert_eq!(turns.end_current_turn_and_advance(&mut ctx).unwrap(), Some(expected));
    }
    assert_eq!(turns.turn_number(), 4);
  }

  #[test]
  fn advancing_before_begin_is_an_error() {
    let mut turns = manager(vec![1, 2]);
    let mut ctx = Ctx::new();
    assert!(turns.end_current_turn_and_advance(&mut ctx).is_err());
  }

  #[test]
  fn notices_name_both_sides_of_the_handoff() {
    let mut turns = manager(vec![1, 2]);
    let mut ctx = Ctx::new();
    turns.begin(&mut ctx);
    turns.end_current_turn_and_advance(&mut ctx).unwrap();

    let notices = notices(ctx);
    assert_eq!(notices[0].new_turn_actor, Some(1));
    assert_eq!(notices[0].previous_turn_actor, None);
    assert_eq!(notices[1].new_turn_actor, Some(2));
    assert_eq!(notices[1].previous_turn_actor, Some(1));
  }

  #[test]
  fn removing_the_active_actor_passes_play_to_the_next() {
    let mut turns = manager(vec![1, 2, 3]);
    let mut ctx = Ctx::new();
    turns.begin(&mut ctx);
    turns.end_current_turn_and_advance(&mut ctx).unwrap(); // player 2's turn

    assert!(turns.remove_actor(&2), "player 2 disconnects mid-turn");
    assert_eq!(turns.current_turn_actor(), Some(3), "play continues, not stalls");
  }

  #[test]
  fn removing_an_earlier_actor_keeps_the_current_turn() {
    let mut turns = manager(vec![1, 2, 3]);
    let mut ctx = Ctx::new();
    turns.begin(&mut ctx);
    turns.end_current_turn_and_advance(&mut ctx).unwrap();
    turns.end_current_turn_and_advance(&mut ctx).unwrap(); // player 3's turn

    turns.remove_actor(&1);
    assert_eq!(turns.current_turn_actor(), Some(3), "still player 3's turn");
  }

  #[test]
  fn removing_the_last_actor_at_the_end_wraps_to_the_start() {
    let mut turns = manager(vec![1, 2]);
    let mut ctx = Ctx::new();
    turns.begin(&mut ctx);
    turns.end_current_turn_and_advance(&mut ctx).unwrap(); // player 2, the last slot

    turns.remove_actor(&2);
    assert_eq!(turns.current_turn_actor(), Some(1));
  }

  #[test]
  fn emptying_the_roster_leaves_no_active_turn() {
    let mut turns = manager(vec![1]);
    let mut ctx = Ctx::new();
    turns.begin(&mut ctx);

    turns.remove_actor(&1);
    assert_eq!(turns.current_turn_actor(), None);
    assert!(turns.end_current_turn_and_advance(&mut ctx).is_err());
  }

  #[test]
  fn restarting_seats_the_first_actor_and_starts_the_count_over() {
    let mut turns = manager(vec![1, 2, 3]);
    let mut ctx = Ctx::new();
    turns.begin(&mut ctx);
    turns.end_current_turn_and_advance(&mut ctx).unwrap();
    turns.end_current_turn_and_advance(&mut ctx).unwrap(); // player 3, turn 3

    assert_eq!(turns.restart(&mut ctx), Some(1));
    assert_eq!(turns.turn_number(), 1, "the order began again, so the count did too");
  }

  #[test]
  fn restarting_is_what_begin_cannot_do() {
    // `begin` returns early while a turn is active, which is every moment after
    // the last actor of a round plays. Without `restart` the next round would
    // silently resume on whoever went last.
    let mut turns = manager(vec![1, 2, 3]);
    let mut ctx = Ctx::new();
    turns.begin(&mut ctx);
    turns.end_current_turn_and_advance(&mut ctx).unwrap(); // player 2

    assert_eq!(turns.begin(&mut ctx), Some(2), "begin declines to interrupt");
    assert_eq!(turns.restart(&mut ctx), Some(1), "restart does not");
  }

  #[test]
  fn restarting_announces_the_handoff() {
    let mut turns = manager(vec![1, 2]);
    let mut ctx = Ctx::new();
    turns.begin(&mut ctx);
    turns.end_current_turn_and_advance(&mut ctx).unwrap(); // player 2
    turns.restart(&mut ctx);

    let last = notices(ctx).pop().expect("restart emits a notice");
    assert_eq!(last.new_turn_actor, Some(1));
    assert_eq!(last.previous_turn_actor, Some(2), "play came back from player 2");
    assert_eq!(last.turn_number, 1);
  }

  #[test]
  fn restarting_before_the_first_begin_just_begins() {
    let mut turns = manager(vec![7, 8]);
    let mut ctx = Ctx::new();

    assert_eq!(turns.restart(&mut ctx), Some(7));
    assert_eq!(turns.turn_number(), 1);
    assert_eq!(notices(ctx).len(), 1);
  }

  #[test]
  fn restarting_an_empty_roster_does_nothing() {
    let mut turns = manager(vec![]);
    let mut ctx = Ctx::new();

    assert_eq!(turns.restart(&mut ctx), None);
    assert!(ctx.is_empty(), "nobody to announce a turn for");
  }

  #[test]
  fn a_clone_advances_independently_of_the_original() {
    // A game that searches ahead clones its state and re-runs turns in
    // simulation. This is why the notice constructor is a `fn` pointer and not
    // a boxed closure: the boxed version was not `Clone`, so this was
    // impossible to write at all.
    let mut live = manager(vec![1, 2, 3]);
    let mut ctx = Ctx::new();
    live.begin(&mut ctx);

    let mut sim = live.clone();
    sim.end_current_turn_and_advance(&mut ctx).unwrap();
    sim.end_current_turn_and_advance(&mut ctx).unwrap();

    assert_eq!(sim.current_turn_actor(), Some(3), "the simulation ran ahead");
    assert_eq!(live.current_turn_actor(), Some(1), "the real game did not move");
    assert_eq!(live.turn_number(), 1);
  }

  #[test]
  fn time_limit_rides_along_on_every_notice() {
    let mut turns = manager(vec![1, 2]).with_time_limit(Duration::from_secs(30));
    let mut ctx = Ctx::new();
    turns.begin(&mut ctx);
    assert_eq!(notices(ctx)[0].time_limit_for_turn, Some(Duration::from_secs(30)));
  }
}
