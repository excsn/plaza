//! Deferred work that belongs to one occupancy of a phase.
//!
//! Extracted after being hand-written nine times across four examples. Every
//! turn-based example schedules work against its phase (a turn timeout, a
//! rematch, a day's deadline), stamps each event with [`Epoch`], and writes the
//! same guard in its drain loop:
//!
//! ```ignore
//! for due in state.timeouts.tick(state.tick) {
//!   if !state.phase.is_current(due.epoch) { continue; }   // ninth copy
//!   ...
//! }
//! ```
//!
//! [`PhasedScheduler`] owns that pairing: the epoch is captured when the work
//! is scheduled and checked when it comes due, so a stale event is dropped
//! inside [`due`](PhasedScheduler::due) and application events carry no epoch
//! field at all.
//!
//! # Capturing at schedule time is the discipline, kept
//!
//! Every hand-written site carried the same comment: the token is taken *after*
//! the transition, so it names the occupancy the work belongs to. That
//! ordering still matters here, in the same shape: schedule after you
//! transition. What the type removes is the other half of the mistake, an
//! event constructed with one occupancy's token and drained against another's
//! rule.
//!
//! # What stays with the application
//!
//! Only the epoch. `card_table` also asks whether the timed-out player is
//! *still on turn*, and `draft_board`'s reversal makes that an identity check a
//! generation counter would get wrong. Checks like that are the game's, and a
//! block that absorbed them would be deciding game rules.

use std::fmt::Debug;

use tracing::debug;

use crate::common::scheduler::TickEventScheduler;

use super::phases::{Epoch, Phased};

/// A tick scheduler whose every event belongs to one phase occupancy.
///
/// Wraps [`TickEventScheduler`], pairing each event with the [`Epoch`] current
/// at schedule time. [`due`](Self::due) yields only events whose occupancy
/// still holds; the rest are dropped with a debug line, exactly as every
/// hand-written drain did.
#[derive(Debug, Clone, Default)]
pub struct PhasedScheduler<E: Clone + Debug + Send + 'static> {
  inner: TickEventScheduler<(Epoch, E)>,
}

impl<E: Clone + Debug + Send + 'static> PhasedScheduler<E> {
  pub fn new() -> Self {
    Self {
      inner: TickEventScheduler::new(),
    }
  }

  /// Schedules `event` for `delay` ticks from `now`, belonging to the
  /// occupancy `phase` currently holds.
  ///
  /// Call this **after** any transition the work belongs to: the token is
  /// captured here, and a token taken before the transition names the
  /// occupancy that just ended.
  pub fn schedule_after<P>(&mut self, now: u64, delay: u64, phase: &Phased<P>, event: E) {
    self.inner.schedule_after(now, delay, (phase.epoch(), event));
  }

  /// Everything due at `now` whose occupancy still holds.
  ///
  /// Stale events are consumed and dropped: nothing cancelled them, their
  /// token simply stopped matching, which is the whole design. An event that
  /// outlived its phase must not act on the phase that replaced it.
  pub fn due<P>(&mut self, now: u64, phase: &Phased<P>) -> Vec<E> {
    self
      .inner
      .tick(now)
      .into_iter()
      .filter_map(|(epoch, event)| {
        if phase.is_current(epoch) {
          Some(event)
        } else {
          debug!(?event, "deferred work dropped: the phase moved on");
          None
        }
      })
      .collect()
  }

  /// Whether any pending event matches, current or stale.
  pub fn any_pending(&self, predicate: impl Fn(&E) -> bool) -> bool {
    self.inner.any_pending(|(_, event)| predicate(event))
  }

  pub fn is_empty(&self) -> bool {
    self.inner.is_empty()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::common::fsm::OpsQueue;

  #[derive(Clone, Copy, Debug, PartialEq, Eq)]
  enum Season {
    Spring,
    Summer,
  }

  #[derive(Clone, Debug, PartialEq)]
  enum Chore {
    Water,
    Harvest,
  }

  #[derive(Clone, Debug, PartialEq)]
  enum Op {
    Phase(super::super::phases::op_payloads::PhaseChangedNoticePayload<Season>),
  }

  type Ctx = OpsQueue<Op, u32>;

  #[test]
  fn work_scheduled_in_an_occupancy_fires_in_it() {
    let phase = Phased::new(Season::Spring);
    let mut chores: PhasedScheduler<Chore> = PhasedScheduler::new();
    chores.schedule_after(0, 5, &phase, Chore::Water);

    assert!(chores.due(4, &phase).is_empty(), "not due yet");
    assert_eq!(chores.due(5, &phase), vec![Chore::Water]);
  }

  #[test]
  fn work_outlived_by_its_phase_is_dropped_not_fired() {
    // The guard this type exists to own: nine hand-written copies of it across
    // four examples, all this line.
    let mut phase = Phased::new(Season::Spring);
    let mut chores: PhasedScheduler<Chore> = PhasedScheduler::new();
    chores.schedule_after(0, 5, &phase, Chore::Water);

    let mut ctx = Ctx::new();
    phase.transition_to(Season::Summer, &mut ctx, Op::Phase);
    assert!(chores.due(5, &phase).is_empty(), "spring's chore does not run in summer");
  }

  #[test]
  fn a_stale_event_does_not_block_a_current_one() {
    let mut phase = Phased::new(Season::Spring);
    let mut chores: PhasedScheduler<Chore> = PhasedScheduler::new();
    chores.schedule_after(0, 5, &phase, Chore::Water);

    let mut ctx = Ctx::new();
    phase.transition_to(Season::Summer, &mut ctx, Op::Phase);
    chores.schedule_after(0, 5, &phase, Chore::Harvest);

    assert_eq!(chores.due(5, &phase), vec![Chore::Harvest], "only summer's chore survives");
  }

  #[test]
  fn returning_to_the_same_phase_is_a_new_occupancy() {
    // The property that makes the token a token rather than a phase compare:
    // spring-again is not the spring the chore was scheduled in.
    let mut phase = Phased::new(Season::Spring);
    let mut chores: PhasedScheduler<Chore> = PhasedScheduler::new();
    chores.schedule_after(0, 5, &phase, Chore::Water);

    let mut ctx = Ctx::new();
    phase.transition_to(Season::Summer, &mut ctx, Op::Phase);
    phase.transition_to(Season::Spring, &mut ctx, Op::Phase);
    assert!(chores.due(5, &phase).is_empty(), "a second spring is not the first");
  }

  #[test]
  fn pending_asks_about_the_event_not_the_token() {
    let phase = Phased::new(Season::Spring);
    let mut chores: PhasedScheduler<Chore> = PhasedScheduler::new();
    assert!(chores.is_empty());
    chores.schedule_after(0, 5, &phase, Chore::Water);
    assert!(chores.any_pending(|c| *c == Chore::Water));
    assert!(!chores.any_pending(|c| *c == Chore::Harvest));
  }
}
