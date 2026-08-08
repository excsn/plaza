//! A [`TurnManager`] whose order reverses at the end of every pass.
//!
//! Written to answer one question: is `TurnManager` a seam, or a description of
//! the one thing that implements it? `RoundRobinTurnManager` has been its only
//! implementation, so nothing had ever tried.
//!
//! # What it found, and what changed as a result
//!
//! The advance fit from the start. `end_current_turn_and_advance` returning the
//! *same* actor at a reversal is allowed by a contract that promises the next
//! turn rather than a different holder of it.
//!
//! Two things did not, and the trait moved rather than this type working around
//! them. It held **two** methods while every consumer called five, so `begin`,
//! `restart`, `add_actor` and `remove_actor` were inherent on
//! `RoundRobinTurnManager` alone and a conforming manager could be written that
//! no application could seat or change the roster of. And nothing could report a
//! **pass boundary**, which round-robin hides because its actor changes there;
//! under a snake the actor is the same on both sides, so a caller inferring the
//! boundary from the actor reads it backwards. That is now
//! [`Advanced::PassClosed`](plaza::game_common::flow_control::Advanced).
//!
//! The remaining divergence is deliberate and is why a policy abstraction is not
//! obviously the next step: `remove_actor` at the end of the roster **wraps** in
//! round-robin and **pulls back** here, because a snake at the end is about to
//! turn around rather than start over. Two implementations differing in advance,
//! in removal fixup, and in what `restart` resets is more variation than a
//! single `next(index)` hook would carry.
//!
//! # The reversal, and why it is not a wrap
//!
//! Round-robin wraps: after the last actor comes the first. A snake **reverses**,
//! so the actor at the end of the order takes two turns in a row, one closing a
//! pass and one opening the next. That is the whole difference, it is the reason
//! a draft uses it (picking last is compensated by picking first next round), and
//! it is the boundary a wrapping manager cannot express at all.

use std::fmt::Debug;
use std::marker::PhantomData;

use plaza::agent::AgentId;
use plaza::common::fsm::FsmContext;
use plaza::game_common::flow_control::turns::op_payloads::TurnChangedNoticePayload;
use plaza::game_common::flow_control::{Advanced, TurnManager};
use plaza::session::TargetedOp;

/// Turn order that runs to the end of the roster and then back along it.
pub struct SnakeTurnManager<Op, AppID: AgentId, TurnActorId: Clone + Debug> {
  actors: Vec<TurnActorId>,
  /// Index into `actors`; `None` before the first turn begins.
  current: Option<usize>,
  /// Which way the cursor is travelling. Flips at each end of the roster.
  descending: bool,
  turn_number: u32,
  /// Turns taken in the current pass, so a caller can tell a pass ended without
  /// the trait having a way to say so.
  in_pass: u32,
  notice: fn(TurnChangedNoticePayload<TurnActorId>) -> Op,
  _phantom: PhantomData<fn() -> AppID>,
}

impl<Op, AppID: AgentId, TurnActorId: Clone + Debug> Clone for SnakeTurnManager<Op, AppID, TurnActorId> {
  fn clone(&self) -> Self {
    Self {
      actors: self.actors.clone(),
      current: self.current,
      descending: self.descending,
      turn_number: self.turn_number,
      in_pass: self.in_pass,
      notice: self.notice,
      _phantom: PhantomData,
    }
  }
}

impl<Op, AppID: AgentId, TurnActorId: Clone + Debug> Debug for SnakeTurnManager<Op, AppID, TurnActorId> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("SnakeTurnManager")
      .field("actors", &self.actors)
      .field("current", &self.current)
      .field("descending", &self.descending)
      .field("turn_number", &self.turn_number)
      .finish()
  }
}

impl<Op, AppID: AgentId, TurnActorId: Clone + Debug + PartialEq> SnakeTurnManager<Op, AppID, TurnActorId> {
  pub fn new(actors: Vec<TurnActorId>, notice: fn(TurnChangedNoticePayload<TurnActorId>) -> Op) -> Self {
    Self {
      actors,
      current: None,
      descending: false,
      turn_number: 0,
      in_pass: 0,
      notice,
      _phantom: PhantomData,
    }
  }

  pub fn actors(&self) -> &[TurnActorId] {
    &self.actors
  }

  pub fn turn_number(&self) -> u32 {
    self.turn_number
  }

  /// Turns taken in the current pass, counting from one.
  ///
  /// The trait returns only the next actor, so a caller cannot learn from it
  /// that a pass just closed. A snake makes that worse than round-robin does:
  /// the actor is the same on both sides of the boundary, so "did it change"
  /// cannot stand in for "did the pass end" either.
  pub fn in_pass(&self) -> u32 {
    self.in_pass
  }

  /// Whether the order is currently travelling back along the roster.
  pub fn descending(&self) -> bool {
    self.descending
  }

  fn emit(&self, context: &mut dyn FsmContext<Op, AppID>, previous: Option<TurnActorId>) {
    let payload = TurnChangedNoticePayload {
      new_turn_actor: self.current_turn_actor(),
      previous_turn_actor: previous,
      turn_number: self.turn_number,
      time_limit_for_turn: None,
    };
    context.ops_q().push(TargetedOp::new_system_all(vec![(self.notice)(payload)]));
  }
}

impl<Op, AppID, TurnActorId> TurnManager<Op, AppID, TurnActorId> for SnakeTurnManager<Op, AppID, TurnActorId>
where
  AppID: AgentId,
  TurnActorId: Clone + Debug + PartialEq,
{
  fn current_turn_actor(&self) -> Option<TurnActorId> {
    self.current.and_then(|i| self.actors.get(i).cloned())
  }

  /// Seats the first actor. No-op once a turn is active or on an empty roster.
  fn begin(&mut self, context: &mut dyn FsmContext<Op, AppID>) -> Option<TurnActorId> {
    if self.current.is_some() || self.actors.is_empty() {
      return self.current_turn_actor();
    }
    self.current = Some(0);
    self.descending = false;
    self.turn_number = 1;
    self.in_pass = 1;
    self.emit(context, None);
    self.current_turn_actor()
  }

  /// Back to the first seat, travelling forwards, counts from one.
  ///
  /// A draft that restarts wants this rather than a reversal: a new draft begins
  /// at the top of the order however the last one ended.
  fn restart(&mut self, context: &mut dyn FsmContext<Op, AppID>) -> Option<TurnActorId> {
    if self.actors.is_empty() {
      self.current = None;
      return None;
    }
    let previous = self.current_turn_actor();
    self.current = Some(0);
    self.descending = false;
    self.turn_number = 1;
    self.in_pass = 1;
    self.emit(context, previous);
    self.current_turn_actor()
  }

  fn add_actor(&mut self, actor: TurnActorId) {
    self.actors.push(actor);
  }

  /// Removes an actor, leaving the turn on whoever now holds that slot.
  ///
  /// Direction is preserved, and a cursor past the end is pulled back to the
  /// last seat rather than wrapped to the first: a snake at the end of its
  /// roster is about to turn around, not about to start over.
  fn remove_actor(&mut self, actor: &TurnActorId) -> bool {
    let Some(index) = self.actors.iter().position(|a| a == actor) else {
      return false;
    };
    self.actors.remove(index);

    if self.actors.is_empty() {
      self.current = None;
      return true;
    }
    if let Some(current) = self.current {
      let shifted = if index < current { current - 1 } else { current };
      self.current = Some(shifted.min(self.actors.len() - 1));
    }
    true
  }


  /// Steps along the order, reversing rather than wrapping at either end.
  ///
  /// At a reversal the returned actor is the **same** one that just played. The
  /// trait's contract allows it, since it promises the next actor rather than a
  /// different one, and a draft depends on it.
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
    let last = self.actors.len() - 1;

    // A single actor is at both ends at once, so it neither steps nor reverses.
    if last == 0 {
      self.current = Some(0);
    } else if self.descending {
      if current == 0 {
        self.descending = false;
      } else {
        self.current = Some(current - 1);
      }
    } else if current == last {
      self.descending = true;
    } else {
      self.current = Some(current + 1);
    }

    // The reversal *is* the pass boundary, and it is the case where the cursor
    // does not move: the same actor closes one pass and opens the next.
    let reversed = self.current == Some(current) && last > 0;
    self.in_pass = if reversed { 1 } else { self.in_pass + 1 };
    self.turn_number = self.turn_number.saturating_add(1);
    self.emit(context, previous);

    let actor = self
      .current_turn_actor()
      .ok_or_else(|| "the turn order lost its actor while advancing".to_string())?;
    Ok(if reversed || last == 0 {
      Advanced::PassClosed(actor)
    } else {
      Advanced::Next(actor)
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use plaza::common::fsm::OpsQueue;

  #[derive(Clone, Debug, PartialEq)]
  enum TestOp {
    Turn(TurnChangedNoticePayload<u32>),
  }

  type Ctx = OpsQueue<TestOp, u32>;

  fn order(actors: Vec<u32>) -> SnakeTurnManager<TestOp, u32, u32> {
    SnakeTurnManager::new(actors, TestOp::Turn)
  }

  /// Walks the order `steps` times, collecting whoever held each turn.
  fn walk(turns: &mut SnakeTurnManager<TestOp, u32, u32>, steps: usize) -> Vec<u32> {
    let mut ctx = Ctx::new();
    let mut seen = vec![turns.begin(&mut ctx).unwrap()];
    for _ in 0..steps {
      seen.push(turns.end_current_turn_and_advance(&mut ctx).unwrap().into_actor());
    }
    seen
  }

  #[test]
  fn the_order_reverses_rather_than_wrapping() {
    // The whole example in one assertion. Round-robin would give 1,2,3,1,2,3.
    let mut turns = order(vec![1, 2, 3]);
    assert_eq!(walk(&mut turns, 8), vec![1, 2, 3, 3, 2, 1, 1, 2, 3]);
  }

  #[test]
  fn the_actor_at_a_reversal_holds_two_turns_in_a_row() {
    // `end_current_turn_and_advance` returning the *same* actor is what the
    // trait's contract has to permit for a snake to be writable at all. It
    // promises the next actor, not a different one.
    let mut turns = order(vec![1, 2, 3]);
    let mut ctx = Ctx::new();
    turns.begin(&mut ctx);
    turns.end_current_turn_and_advance(&mut ctx).unwrap();
    let last_of_the_pass = turns.end_current_turn_and_advance(&mut ctx).unwrap();
    let first_of_the_next = turns.end_current_turn_and_advance(&mut ctx).unwrap();
    assert_eq!(last_of_the_pass, Advanced::Next(3));
    assert_eq!(
      first_of_the_next,
      Advanced::PassClosed(3),
      "the turn did not move, the direction did, and only the manager can say so"
    );
  }

  #[test]
  fn a_pass_boundary_is_not_visible_from_the_returned_actor() {
    // Why the application counts picks instead of watching the manager: the
    // actor is unchanged across the boundary, so "did it change" reports the
    // opposite of the truth exactly where it matters.
    let mut turns = order(vec![1, 2, 3]);
    let mut ctx = Ctx::new();
    turns.begin(&mut ctx);
    turns.end_current_turn_and_advance(&mut ctx).unwrap();
    turns.end_current_turn_and_advance(&mut ctx).unwrap();
    assert_eq!(turns.in_pass(), 3, "third of the first pass");

    let boundary = turns.end_current_turn_and_advance(&mut ctx).unwrap();
    assert_eq!(turns.in_pass(), 1, "and first of the second, as the same actor");
    assert!(boundary.pass_closed(), "which the advance reports rather than the caller inferring");
  }

  #[test]
  fn the_direction_is_readable_and_flips_at_each_end() {
    let mut turns = order(vec![1, 2]);
    let mut ctx = Ctx::new();
    turns.begin(&mut ctx);
    assert!(!turns.descending());
    turns.end_current_turn_and_advance(&mut ctx).unwrap();
    turns.end_current_turn_and_advance(&mut ctx).unwrap();
    assert!(turns.descending(), "turned around at the end of the roster");
  }

  #[test]
  fn one_actor_neither_steps_nor_reverses() {
    let mut turns = order(vec![7]);
    assert_eq!(walk(&mut turns, 3), vec![7, 7, 7, 7]);
  }

  #[test]
  fn an_empty_order_refuses_rather_than_panicking() {
    let mut turns: SnakeTurnManager<TestOp, u32, u32> = order(Vec::new());
    let mut ctx = Ctx::new();
    assert_eq!(turns.begin(&mut ctx), None);
    assert!(turns.end_current_turn_and_advance(&mut ctx).is_err());
  }

  #[test]
  fn advancing_before_beginning_says_so() {
    let mut turns = order(vec![1, 2]);
    let mut ctx = Ctx::new();
    assert!(turns.end_current_turn_and_advance(&mut ctx).is_err(), "no turn is active");
  }

  #[test]
  fn removing_an_actor_leaves_the_turn_on_whoever_holds_the_slot() {
    let mut turns = order(vec![1, 2, 3]);
    let mut ctx = Ctx::new();
    turns.begin(&mut ctx);
    turns.end_current_turn_and_advance(&mut ctx).unwrap();
    assert_eq!(turns.current_turn_actor(), Some(2));

    assert!(turns.remove_actor(&2));
    assert_eq!(turns.current_turn_actor(), Some(3), "the slot now holds the next actor");
  }

  #[test]
  fn removing_the_actor_at_the_end_pulls_back_rather_than_wrapping() {
    // Round-robin wraps to the first seat here. A snake at the end of its
    // roster is about to turn around, so wrapping would skip the reversal.
    let mut turns = order(vec![1, 2, 3]);
    let mut ctx = Ctx::new();
    turns.begin(&mut ctx);
    turns.end_current_turn_and_advance(&mut ctx).unwrap();
    turns.end_current_turn_and_advance(&mut ctx).unwrap();
    assert_eq!(turns.current_turn_actor(), Some(3));

    assert!(turns.remove_actor(&3));
    assert_eq!(turns.current_turn_actor(), Some(2), "the last seat, not the first");
  }

  #[test]
  fn restarting_returns_to_the_top_travelling_forwards() {
    // A new draft is not the next pass of the old one, so `restart` undoes the
    // direction as well as the position.
    let mut turns = order(vec![1, 2, 3]);
    let mut ctx = Ctx::new();
    walk(&mut turns, 3);
    assert!(turns.descending());

    assert_eq!(turns.restart(&mut ctx), Some(1));
    assert!(!turns.descending());
    assert_eq!(turns.turn_number(), 1);
  }

  #[test]
  fn it_is_usable_behind_the_trait_it_implements() {
    // The seam, whole: a caller holding any manager can seat it, read it,
    // advance it, and change its roster. Before the trait was widened this test
    // had to reach past it to a concrete `begin` before it could start.
    let mut concrete = order(vec![1, 2]);
    let mut ctx = Ctx::new();

    // Everything through the trait now, including the seating that used to
    // force a caller back to the concrete type.
    let turns: &mut dyn TurnManager<TestOp, u32, u32> = &mut concrete;
    turns.begin(&mut ctx);
    turns.add_actor(3);
    assert_eq!(turns.current_turn_actor(), Some(1));
    assert_eq!(turns.end_current_turn_and_advance(&mut ctx).unwrap(), Advanced::Next(2));
    assert!(turns.remove_actor(&3));
    assert_eq!(turns.restart(&mut ctx), Some(1));
  }
}
