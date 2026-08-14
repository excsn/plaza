//! Prediction by shared rule: walk the route yourself, check the route, and
//! settle only at rest.
//!
//! The other predictors in this crate re-run inputs against samples. This one
//! is for the games where that is the wrong shape: the client runs the **same
//! deterministic rule** the server runs (a pathfinder, a step function over a
//! derived map), so a click's whole journey is known on both ends the moment
//! it happens, and one op covers a walk longer than any round trip. What is
//! left to prediction is only presentation: a body that sets off now, crosses
//! its squares on the local clock, and never jumps.
//!
//! Four invariants, each one a play-tested bug when it was missing:
//!
//! - **Check routes, not positions.** The two ends are a tick out of phase by
//!   design; comparing squares directly reports that phase as an error and
//!   buries the real signal. The server's positions are spent against the
//!   *route* this client drew.
//! - **Only check a journey both ends started together.** A click mid-walk
//!   makes each end expand the route from wherever it currently is, which is
//!   two different squares, and the honest verdict is "not comparable" rather
//!   than "diverged".
//! - **A click changes where the body is going and never where it is.** The
//!   crossing in progress keeps its continuous start point; granting a free
//!   step per click lets spam outrun the server and be pulled back later,
//!   which reads as rubber-banding nothing caused.
//! - **Reconcile only at rest.** Snapping a walking body to a square the
//!   server happens to be on reads as a rollback and is wrong besides: both
//!   ends are walking to the same place from different starts and will arrive
//!   together. Waiting until the walking is over makes the usual case a no-op.
//!
//! The prerequisite is the crate's first principle at full strength: the rule
//! must be **shared code over shared state**, deterministic on both ends, or
//! every journey diverges and the notice is the only thing on screen.

use std::collections::VecDeque;

/// What a confirmation said about the route being checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Heard {
  /// The server is on this route, possibly a square or two ahead of the last
  /// report.
  OnRoute,
  /// The server walked a different way. The check is over; the body finishes
  /// its own route and [`settle`](RoutePredictor::settle) takes the server's
  /// square at rest. Worth a counter and a loud notice, because a shared rule
  /// that diverges is a bug and not weather.
  Diverged,
  /// Nothing was being checked: the journey did not start from a shared
  /// square, or the route ran out mid-check. Not an error and not a
  /// confirmation.
  Unchecked,
}

/// A body that walks its own route on the local clock, against a server
/// walking the same rule.
///
/// `P` is a square: a tile, a node, whatever the shared rule routes over.
/// `point` maps it to the continuous plane the body is drawn on, and is a
/// plain `fn` so no closure bounds are imposed.
#[derive(Clone, Debug)]
pub struct RoutePredictor<P: Copy + PartialEq> {
  point: fn(&P) -> [f32; 2],
  /// Where this client believes the body is, in whole squares.
  pub predicted: P,
  /// Where the server last said it was.
  pub confirmed: P,
  /// Squares left to walk, consumed by the local clock.
  plan: VecDeque<P>,
  /// The same squares, spent against the server's confirmations.
  expect: VecDeque<P>,
  /// Only a journey both ends started together is checked at all.
  checking: bool,
  /// The crossing's start **point**, not a square: a mid-crossing click must
  /// not teleport the fraction already walked.
  from: [f32; 2],
  step_ms: u64,
  stepped_ms: u64,
  next_step_ms: u64,
  seeded: bool,
  pub confirmations: u64,
  pub diverged: u64,
}

impl<P: Copy + PartialEq> RoutePredictor<P> {
  pub fn new(initial: P, point: fn(&P) -> [f32; 2], step_ms: u64) -> Self {
    Self {
      point,
      predicted: initial,
      confirmed: initial,
      plan: VecDeque::new(),
      expect: VecDeque::new(),
      checking: false,
      from: point(&initial),
      step_ms: step_ms.max(1),
      stepped_ms: 0,
      next_step_ms: 0,
      seeded: false,


      confirmations: 0,
      diverged: 0,
    }
  }

  /// The server's tick length, which is the local walking cadence too. A live
  /// value: chase the frame's `tick_ms` with it.
  pub fn set_step_ms(&mut self, step_ms: u64) {
    self.step_ms = step_ms.max(1);
  }

  /// Puts the body somewhere without walking there: the seat assignment, a
  /// respawn, a teleport. The one move that is allowed to jump.
  pub fn jump_to(&mut self, at: P, now_ms: u64) {
    self.predicted = at;
    self.confirmed = at;
    self.from = (self.point)(&at);
    self.plan.clear();
    self.expect.clear();
    self.checking = false;
    self.stepped_ms = now_ms;
    self.next_step_ms = now_ms + self.step_ms;
    self.seeded = true;
  }

  pub fn is_seeded(&self) -> bool {
    self.seeded
  }

  /// Takes a route the shared rule produced, and touches nothing about where
  /// the body *is*.
  ///
  /// `checkable` marks a journey whose server twin expands from the same
  /// square this one does: at rest, nothing owed, confirmed and predicted
  /// agreeing. A click mid-walk fails that and simply is not checked, because
  /// each end expands from a different square and a comparison would report
  /// the design as a bug. An op the rule answers differently per end (a chase
  /// of something moving) should pass `checkable = false` outright.
  pub fn set_out(&mut self, route: impl IntoIterator<Item = P>, checkable: bool, now_ms: u64) {
    let was_walking = !self.plan.is_empty() || self.crossing(now_ms);
    self.checking = checkable
      && self.plan.is_empty()
      && self.expect.is_empty()
      && self.confirmed == self.predicted;
    self.plan.clear();
    self.expect.clear();
    for square in route {
      self.plan.push_back(square);
      self.expect.push_back(square);
    }
    // A body at rest sets off now; a walking one changes course on its own
    // next step. No free step either way: granting one per click lets spam
    // outrun the server and be pulled back at rest.
    if !was_walking {
      self.next_step_ms = now_ms;
    }
  }

  /// Walks the local clock forward, consuming up to `steps_per_tick` squares
  /// each time a tick of it elapses. Two is a run; the rule is the server's.
  pub fn advance(&mut self, now_ms: u64, steps_per_tick: u32) {
    if !self.seeded || self.plan.is_empty() {
      return;
    }
    while now_ms >= self.next_step_ms && !self.plan.is_empty() {
      // The next crossing starts wherever the body is drawn right now, which
      // is what keeps the picture continuous whatever the route has been
      // doing.
      self.from = self.drawn(now_ms);
      for _ in 0..steps_per_tick.max(1) {
        if let Some(next) = self.plan.pop_front() {
          self.predicted = next;
        }
      }
      self.stepped_ms = now_ms;
      self.next_step_ms = now_ms + self.step_ms;
    }
  }

  /// Where the body is drawn, between the crossing's start point and the
  /// predicted square.
  pub fn drawn(&self, now_ms: u64) -> [f32; 2] {
    let t = (now_ms.saturating_sub(self.stepped_ms) as f32 / self.step_ms as f32).clamp(0.0, 1.0);
    let to = (self.point)(&self.predicted);
    [
      self.from[0] + (to[0] - self.from[0]) * t,
      self.from[1] + (to[1] - self.from[1]) * t,
    ]
  }

  /// Whether the body has squares left or is still crossing the last one.
  ///
  /// The clock is the half that is easy to leave out, and leaving it out is a
  /// walk cycle that never stops: the crossing's start is only rewritten on a
  /// step, so an arrived body holds a stale one for ever and walks on the
  /// spot until the next click.
  pub fn walking(&self, now_ms: u64) -> bool {
    !self.plan.is_empty() || self.crossing(now_ms)
  }

  /// Whether the body is mid-crossing right now.
  pub fn crossing(&self, now_ms: u64) -> bool {
    let to = (self.point)(&self.predicted);
    let (dx, dy) = (to[0] - self.from[0], to[1] - self.from[1]);
    dx * dx + dy * dy > 1e-4 && now_ms.saturating_sub(self.stepped_ms) < self.step_ms
  }

  /// The direction of the crossing in progress, for facing a body the way it
  /// is going. `None` when it is not going anywhere.
  pub fn heading(&self, now_ms: u64) -> Option<[f32; 2]> {
    if !self.crossing(now_ms) {
      return None;
    }
    let to = (self.point)(&self.predicted);
    Some([to[0] - self.from[0], to[1] - self.from[1]])
  }

  /// The squares left to walk, for drawing the route.
  pub fn plan(&self) -> impl Iterator<Item = &P> + '_ {
    self.plan.iter()
  }

  pub fn plan_is_empty(&self) -> bool {
    self.plan.is_empty()
  }

  /// Checks the server's square against the route this client drew.
  ///
  /// Not against the client's current square: the two are a tick out of phase
  /// by design, and counting that as an error would bury the thing this is
  /// for. `slack` is the most squares one report may advance, which is the
  /// server's `steps_per_tick`: a run covers two squares a tick and the first
  /// of them is a square nothing ever reports.
  pub fn confirm(&mut self, at: P, slack: u32) -> Heard {
    if at == self.confirmed {
      return Heard::Unchecked;
    }
    self.confirmed = at;
    if !self.checking {
      // Nothing to check against, and nothing to correct either: the body
      // keeps walking the route it drew, and `settle` takes the server's
      // square once it has stopped.
      return Heard::Unchecked;
    }
    self.confirmations += 1;
    for _ in 0..slack.max(1) {
      match self.expect.pop_front() {
        Some(next) if next == at => return Heard::OnRoute,
        Some(_) => continue,
        None => {
          self.checking = false;
          return Heard::Unchecked;
        }
      }
    }
    self.diverged += 1;
    self.checking = false;
    Heard::Diverged
  }

  /// Abandons the route where the body stands: the server refused it, or the
  /// world made it moot. The drawn position is preserved; `at` is where the
  /// body now is in whole squares, usually the server's answer.
  pub fn abandon(&mut self, at: P, now_ms: u64) {
    self.from = self.drawn(now_ms);
    self.predicted = at;
    self.plan.clear();
    self.expect.clear();
    self.checking = false;
    self.stepped_ms = now_ms;
    self.next_step_ms = now_ms + self.step_ms;
  }

  /// Takes the server's square once the body has stopped and has nothing left
  /// to walk. The whole of reconciliation, and deliberately not a per-tick
  /// correction; returns whether anything moved, which is rare by design,
  /// because by the time the walking is over the two ends agree.
  pub fn settle(&mut self, now_ms: u64) -> bool {
    if !self.seeded || self.walking(now_ms) || self.confirmed == self.predicted {
      return false;
    }
    // Eased across a tick like any other step, because even the rare
    // reconciliation must not look like one.
    self.from = self.drawn(now_ms);
    self.predicted = self.confirmed;
    self.plan.clear();
    self.expect.clear();
    self.stepped_ms = now_ms;
    self.next_step_ms = now_ms + self.step_ms;
    true
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  type Sq = (i16, i16);

  fn point(sq: &Sq) -> [f32; 2] {
    [sq.0 as f32, sq.1 as f32]
  }

  fn line(from: Sq, len: i16) -> Vec<Sq> {
    (1..=len).map(|i| (from.0 + i, from.1)).collect()
  }

  fn predictor() -> RoutePredictor<Sq> {
    let mut p = RoutePredictor::new((0, 0), point, 100);
    p.jump_to((0, 0), 0);
    p
  }

  #[test]
  fn the_client_walks_the_route_the_server_walks() {
    let mut p = predictor();
    let route = line((0, 0), 10);
    p.set_out(route.clone(), true, 0);
    let mut now = 0;
    for square in route {
      now += 100;
      p.advance(now, 1);
      assert_eq!(p.confirm(square, 1), Heard::OnRoute);
    }
    assert_eq!(p.diverged, 0);
    assert_eq!(p.confirmations, 10);
    assert!(!p.walking(now + 200));
  }

  #[test]
  fn a_journey_the_ends_started_apart_is_not_checked() {
    // The second click lands mid-walk: each end expands the route from
    // wherever it is, which is two different squares, and the honest verdict
    // is "not comparable" rather than "diverged".
    let mut p = predictor();
    p.set_out(line((0, 0), 6), true, 0);
    p.advance(150, 1);
    p.set_out(line(p.predicted, 4), true, 150);
    assert_eq!(p.confirm((2, 0), 1), Heard::Unchecked);
    assert_eq!(p.diverged, 0, "a phase difference is not a divergence");
  }

  #[test]
  fn a_click_changes_where_the_body_is_going_and_never_where_it_is() {
    let mut p = predictor();
    p.set_out(line((0, 0), 6), true, 0);
    p.advance(150, 1);
    let before = p.drawn(150);
    p.set_out(vec![(1, 1), (2, 2)], true, 150);
    let after = p.drawn(150);
    assert_eq!(before, after, "the drawn body moved on a click");
  }

  #[test]
  fn the_drawn_body_never_jumps_however_fast_the_clicks_come() {
    // Routes of adjacent squares, like a rule produces: the property is that
    // however the route keeps changing, the picture stays continuous.
    let mut p = predictor();
    let mut worst = 0.0f32;
    let mut last = p.drawn(0);
    for frame in 1..200u64 {
      let now = frame * 50;
      if frame % 3 == 0 {
        let dir: i16 = if (frame / 3) % 2 == 0 { 1 } else { -1 };
        let route: Vec<Sq> = (1..=4).map(|i| (p.predicted.0 + i * dir, p.predicted.1)).collect();
        p.set_out(route, true, now);
      }
      p.advance(now, 1);
      let drawn = p.drawn(now);
      let (dx, dy) = (drawn[0] - last[0], drawn[1] - last[1]);
      worst = worst.max((dx * dx + dy * dy).sqrt());
      last = drawn;
    }
    // A step is one square per 100ms tick, so a 50ms frame draws at most half
    // a square of progress plus whatever a mid-crossing redirect finishes.
    assert!(worst < 0.75, "the body jumped {worst} squares in one frame");
  }

  #[test]
  fn a_run_advances_two_squares_and_the_unreported_one_is_not_a_divergence() {
    let mut p = predictor();
    p.set_out(line((0, 0), 8), true, 0);
    let mut now = 0;
    for reported in [2i16, 4, 6, 8] {
      now += 100;
      p.advance(now, 2);
      assert_eq!(p.confirm((reported, 0), 2), Heard::OnRoute, "at {reported}");
    }
    assert_eq!(p.diverged, 0);
  }

  #[test]
  fn a_different_route_is_a_divergence_said_once() {
    let mut p = predictor();
    p.set_out(line((0, 0), 4), true, 0);
    p.advance(100, 1);
    assert_eq!(p.confirm((0, 1), 1), Heard::Diverged, "the server went another way");
    assert_eq!(p.diverged, 1);
    assert_eq!(p.confirm((0, 2), 1), Heard::Unchecked, "and the check is over");
    assert_eq!(p.diverged, 1);
  }

  #[test]
  fn settling_happens_only_at_rest() {
    let mut p = predictor();
    p.set_out(line((0, 0), 3), true, 0);
    p.advance(100, 1);
    p.confirm((5, 5), 1);
    assert!(!p.settle(150), "a walking body is not settled");
    let mut now = 150;
    while p.walking(now) {
      now += 100;
      p.advance(now, 1);
    }
    assert!(p.settle(now), "at rest the server's square is taken");
    assert_eq!(p.predicted, (5, 5));
    assert!(!p.settle(now + 100), "and settling is idempotent");
  }

  #[test]
  fn an_arrived_body_stops_walking() {
    // The clock half of `walking`: an arrived body holds a stale crossing
    // start for ever, and without the clock it walks on the spot until the
    // next click.
    let mut p = predictor();
    p.set_out(vec![(1, 0)], true, 0);
    p.advance(100, 1);
    assert!(p.walking(150), "mid-crossing is walking");
    assert!(!p.walking(300), "arrived is not");
  }

  #[test]
  fn abandoning_a_route_keeps_the_picture_continuous() {
    let mut p = predictor();
    p.set_out(line((0, 0), 6), true, 0);
    p.advance(150, 1);
    let before = p.drawn(150);
    p.abandon((0, 0), 150);
    let after = p.drawn(150);
    let (dx, dy) = (after[0] - before[0], after[1] - before[1]);
    assert!((dx * dx + dy * dy).sqrt() < 1e-4, "a refusal must not teleport the body");
    assert!(p.plan_is_empty());
  }
}
