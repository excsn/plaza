//! Traits and operation payloads for managing turns in turn-based games.

use crate::agent::AgentId;
use crate::common::fsm::FsmContext;
use crate::session::TargetedOp;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::time::Duration;

/// What one advance did to the order.
///
/// The variant carries the fact a caller would otherwise have to reconstruct,
/// and reconstruct wrongly: whether the order just completed a pass over its
/// roster. The manager is the only thing that knows, because *how* a pass ends
/// is the thing implementations differ about. Round-robin wraps; a snake
/// reverses and hands the same actor two turns in a row, so a caller comparing
/// the new actor against the old gets the opposite of the truth exactly at the
/// boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Advanced<TurnActorId> {
  /// The order moved on inside the current pass.
  Next(TurnActorId),
  /// The order moved on and a pass over the roster closed.
  PassClosed(TurnActorId),
}

impl<TurnActorId> Advanced<TurnActorId> {
  /// Whoever is now on turn, whichever happened.
  pub fn actor(&self) -> &TurnActorId {
    match self {
      Advanced::Next(actor) | Advanced::PassClosed(actor) => actor,
    }
  }

  pub fn into_actor(self) -> TurnActorId {
    match self {
      Advanced::Next(actor) | Advanced::PassClosed(actor) => actor,
    }
  }

  /// Whether a pass over the roster just completed.
  pub fn pass_closed(&self) -> bool {
    matches!(self, Advanced::PassClosed(_))
  }
}

/// Whose turn it is, and how that moves.
///
/// - `Op`: The application's operation type.
/// - `AppID`: The application's `AgentId` type.
/// - `TurnActorId`: whose turn it is (a `PlayerId`, a `TeamId`, a unit).
///
/// # This is a conformance target, not a dispatch mechanism
///
/// Nothing in this workspace holds a `dyn TurnManager`, and probably nothing
/// will: a game knows which order it plays in. What the trait is for is the
/// question "I am writing initiative order of my own, what must it provide?",
/// and it is sized to answer that rather than to be called through.
///
/// It was sized wrongly until [`draft_board`] wrote the second implementation
/// and found out. For a long time this held two methods while every consumer
/// called five, so a conforming manager could be written that no application
/// could actually seat, restart, or change the roster of. Seating and roster
/// are not optional; a manager that cannot do them is not usable, and the trait
/// now says so.
///
/// [`draft_board`]: https://github.com/excsn/plaza/tree/main/examples/draft_board
pub trait TurnManager<Op, AppID: AgentId, TurnActorId> {
  /// Whose turn it currently is, or `None` before the order has begun.
  fn current_turn_actor(&self) -> Option<TurnActorId>;

  /// Seats the first actor and emits a notice. No-op once a turn is active.
  fn begin(&mut self, context: &mut dyn FsmContext<Op, AppID>) -> Option<TurnActorId>;

  /// Returns to the start of the order and counts from one again.
  ///
  /// What "the start" costs is the implementation's business: round-robin moves
  /// a cursor, a snake also resets its direction. Same intent, different
  /// mechanics, which is what a trait method is for.
  fn restart(&mut self, context: &mut dyn FsmContext<Op, AppID>) -> Option<TurnActorId>;

  /// Adds an actor to the order without disturbing the current turn.
  fn add_actor(&mut self, actor: TurnActorId);

  /// Removes an actor, e.g. one who disconnected. Returns whether it was there.
  ///
  /// Where the turn lands afterwards is the implementation's business too, and
  /// the two shipped answers genuinely differ: round-robin wraps to the first
  /// seat, a snake pulls back to the last, because a snake at the end of its
  /// roster is about to turn around rather than start over.
  fn remove_actor(&mut self, actor: &TurnActorId) -> bool;

  /// Ends the current turn and moves the order on, emitting a
  /// [`TurnChangedNoticePayload`](op_payloads::TurnChangedNoticePayload).
  ///
  /// Returns [`Advanced`], which says whether a pass closed as well as who is
  /// now on turn. `Err` if the roster is empty or no turn has begun.
  ///
  /// **The next actor may be the same actor.** This promises the next turn, not
  /// a different holder of it, and a snake depends on the difference.
  fn end_current_turn_and_advance(
    &mut self,
    context: &mut dyn FsmContext<Op, AppID>,
  ) -> Result<Advanced<TurnActorId>, String>;
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

  /// Starts the first turn, emitting a notice. No-op if the roster is empty or
  /// a turn is already active.
  fn begin(&mut self, context: &mut dyn FsmContext<Op, AppID>) -> Option<TurnActorId> {
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
  fn restart(&mut self, context: &mut dyn FsmContext<Op, AppID>) -> Option<TurnActorId> {
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
  fn add_actor(&mut self, actor: TurnActorId) {
    self.actors.push(actor);
  }

  /// Removes an actor, e.g. a player who disconnected.
  ///
  /// Removing the actor whose turn it is leaves the turn pointing at whoever now
  /// occupies that slot, so play continues rather than stalling on someone who
  /// left. Returns whether the actor was present.
  fn remove_actor(&mut self, actor: &TurnActorId) -> bool {
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

  /// Advances to the next actor, wrapping at the end of the order.
  ///
  /// The wrap is the pass boundary, so that is where [`Advanced::PassClosed`]
  /// is reported.
  fn end_current_turn_and_advance(
    &mut self,
    context: &mut dyn FsmContext<Op, AppID>,
  ) -> Result<Advanced<TurnActorId>, String> {
    if self.actors.is_empty() {
      return Err("no actors in the turn order".to_string());
    }
    let Some(current) = self.current else {
      return Err("no turn is active; call begin() first".to_string());
    };

    let previous = self.actors.get(current).cloned();
    let next = (current + 1) % self.actors.len();
    let wrapped = next == 0;
    self.current = Some(next);
    self.turn_number = self.turn_number.saturating_add(1);
    self.emit_notice(context, previous);

    let actor = self
      .current_turn_actor()
      .ok_or_else(|| "the turn order lost its actor while advancing".to_string())?;
    Ok(if wrapped {
      Advanced::PassClosed(actor)
    } else {
      Advanced::Next(actor)
    })
  }
}

/// Defines common operation payloads related to game turns.
pub mod op_payloads {
  pub use plaza_wire::flow_payloads::{EndTurnRequestPayload, TurnChangedNoticePayload};
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
      let moved = turns.end_current_turn_and_advance(&mut ctx).unwrap();
      assert_eq!(*moved.actor(), expected);
    }
    assert_eq!(turns.turn_number(), 4);
  }

  #[test]
  fn the_wrap_is_reported_as_a_closed_pass() {
    // The fact two examples were reconstructing by counting: only the manager
    // knows where its pass ends, because how a pass ends is what implementations
    // differ about.
    let mut turns = manager(vec![1, 2, 3]);
    let mut ctx = Ctx::new();
    turns.begin(&mut ctx);

    assert_eq!(turns.end_current_turn_and_advance(&mut ctx).unwrap(), Advanced::Next(2));
    assert_eq!(turns.end_current_turn_and_advance(&mut ctx).unwrap(), Advanced::Next(3));
    assert_eq!(
      turns.end_current_turn_and_advance(&mut ctx).unwrap(),
      Advanced::PassClosed(1),
      "back to the top is the end of a pass"
    );
  }

  #[test]
  fn a_single_actor_closes_a_pass_every_turn() {
    let mut turns = manager(vec![7]);
    let mut ctx = Ctx::new();
    turns.begin(&mut ctx);
    assert!(turns.end_current_turn_and_advance(&mut ctx).unwrap().pass_closed());
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
