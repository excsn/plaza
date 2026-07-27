//! Tick-addressed input buffering: the server side of "two players who
//! pressed together execute together, whatever their ping".
//!
//! A client does not send "move now". It names **the server's own tick** its
//! input is meant for, computed from its clock estimate plus the playout depth
//! the server advertised, and the server buffers the input until that tick
//! runs. Applied on arrival instead, the nearer player gets its ping
//! difference as free head start, and anything decided by who-was-where-first
//! is decided by the network.
//!
//! Why a tick and not a timestamp: authority. A timestamp is the client naming
//! a moment, which the server then has to judge plausible, and the judgement
//! needs a shared clock whose error is exactly the slack a liar hides in. A
//! tick is the client naming the server's own unit of time, which is either
//! still open or is not.
//!
//! # The window rejects; it never corrects
//!
//! An input for a tick already simulated is **dropped**, not shifted into the
//! window. Correcting a backdated tick still executes the input, so a lag
//! switch loses the lie and keeps the steering, and the residual advantage is
//! whatever slack the correction allowed. Dropping it means backdating costs
//! the input, and it replaces "how much lying is tolerable" (no good answer)
//! with "is this tick open" (a setting). The cost is real and lands on honest
//! clients too: a link slower than the window loses inputs and rubber-bands,
//! which is why the window is a parameter and [`late`](InputSchedule::late) is
//! a counter worth watching, not a curiosity.
//!
//! # One warning from production: derive the current tick, never count it
//!
//! Everything here takes `current` as a parameter because the schedule must
//! not own a tick counter. A counter kept beside a clock has to be kept in
//! step through every path that touches either, and a world rebuild is such a
//! path: the example this was extracted from preserved the clock and reset the
//! counter, after which every input any client aimed was hundreds of ticks
//! stale and silently refused, permanently. Derive `current` from the
//! simulation clock at the call site.

/// The accepting window, in ticks either side of the current one. A live
/// setting rather than a construction-time constant, because tuning it *is*
/// the experiment: tighter is fairer and rubber-bands slow links sooner.
#[derive(Clone, Copy, Debug)]
pub struct InputWindow {
  /// How many ticks past its named tick an input is still accepted (it then
  /// executes on the next tick to run, and is counted late).
  pub max_late: u64,
  /// How far ahead of the current tick an input may aim before it reads as
  /// parking inputs in the future.
  pub max_early: u64,
}

/// What [`InputSchedule::submit`] decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Submission {
  /// Buffered for its tick.
  Scheduled,
  /// Buffered, but its tick has already passed inside the window: it executes
  /// on the next tick to run. A steady stream of these says the window is too
  /// tight for whoever is connected.
  Late,
  /// Named a tick already simulated and outside the window. Dropped: that
  /// tick is closed, and reopening it is exactly the rewrite of history a lag
  /// switch is trying to buy.
  TickClosed,
  /// Named a tick too far ahead. Dropped.
  TooFarAhead,
}

impl Submission {
  /// Whether the input was buffered at all.
  pub fn accepted(&self) -> bool {
    matches!(self, Submission::Scheduled | Submission::Late)
  }
}

/// One seat's buffered inputs and the counters that judge the window. Keep one
/// per seat, like `DeltaBaseline`.
///
/// ```ignore
/// // On an input op, with `current` derived from the simulation clock:
/// let verdict = schedule.submit(op.tick, op.dir, server.tick(), window);
///
/// // Once per simulation step, before stepping:
/// if let Some(dir) = schedule.execute_due(server.tick()) {
///   held = Some(dir);
/// }
/// ```
#[derive(Clone, Debug, Default)]
pub struct InputSchedule<Input> {
  /// `(execute_at_tick, input)`, ordered by intended time at execution, which
  /// is the property the whole buffer exists for: arrival order is the
  /// network's opinion.
  queue: Vec<(u64, Input)>,
  /// The newest tick anything has executed on, so a client cannot reorder its
  /// own history by walking its named ticks backwards.
  last_executed: Option<u64>,
  accepted: u64,
  late: u64,
  rejected: u64,
}

impl<Input> InputSchedule<Input> {
  pub fn new() -> Self {
    Self {
      queue: Vec::new(),
      last_executed: None,
      accepted: 0,
      late: 0,
      rejected: 0,
    }
  }

  /// Offers an input naming `tick`, judged against `current` (the tick the
  /// simulation is on now, derived from its clock) and the window.
  pub fn submit(&mut self, tick: u64, input: Input, current: u64, window: InputWindow) -> Submission {
    if tick + window.max_late < current {
      self.rejected += 1;
      return Submission::TickClosed;
    }
    if tick > current + window.max_early {
      self.rejected += 1;
      return Submission::TooFarAhead;
    }
    // Never behind something already executed for this seat.
    let mut execute_at = tick;
    if let Some(last) = self.last_executed {
      execute_at = execute_at.max(last);
    }
    self.queue.push((execute_at, input));
    self.accepted += 1;
    if execute_at < current {
      self.late += 1;
      return Submission::Late;
    }
    Submission::Scheduled
  }

  /// The input to apply on tick `current`, if any has come due.
  ///
  /// Call once per simulation step, **per step and not per network frame**:
  /// consuming the queue once per frame collapses everything that arrived
  /// between two ticks onto whichever one happened to run next. Within one
  /// step the newest due input wins, because a held direction is a level
  /// rather than an edge: the older ones it supersedes were never going to be
  /// observable for less than a tick anyway.
  pub fn execute_due(&mut self, current: u64) -> Option<Input> {
    if self.queue.is_empty() {
      return None;
    }
    self.queue.sort_by_key(|(at, _)| *at);
    let due = self.queue.iter().take_while(|(at, _)| *at <= current).count();
    if due == 0 {
      return None;
    }
    let mut applied = None;
    for (_, input) in self.queue.drain(..due) {
      applied = Some(input);
    }
    self.last_executed = Some(current);
    applied
  }

  /// Drops everything buffered, for a seat being vacated. Counters survive:
  /// they describe the session, not the occupant.
  pub fn clear(&mut self) {
    self.queue.clear();
    self.last_executed = None;
  }

  /// Inputs buffered (scheduled plus late). The denominator every other count
  /// needs: rejections mean nothing without knowing how many arrived.
  pub fn accepted(&self) -> u64 {
    self.accepted
  }

  /// Inputs accepted after their tick had passed.
  pub fn late(&self) -> u64 {
    self.late
  }

  /// Inputs dropped for naming a closed or far-future tick.
  pub fn rejected(&self) -> u64 {
    self.rejected
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const WINDOW: InputWindow = InputWindow { max_late: 4, max_early: 30 };

  #[test]
  fn an_input_waits_for_the_tick_it_names() {
    // The whole point: execution time comes from the declaration, not from
    // arrival. Arriving early buys nothing.
    let mut s = InputSchedule::new();
    assert_eq!(s.submit(10, "go", 6, WINDOW), Submission::Scheduled);
    assert_eq!(s.execute_due(8), None, "not its tick yet");
    assert_eq!(s.execute_due(10), Some("go"));
  }

  #[test]
  fn a_closed_tick_is_rejected_not_corrected() {
    // The lag switch. Correcting the tick into the window would still execute
    // the input, so the lie would cost nothing.
    let mut s = InputSchedule::new();
    assert_eq!(s.submit(10, "backdated", 20, WINDOW), Submission::TickClosed);
    assert_eq!(s.execute_due(20), None, "dropped, not shifted");
    assert_eq!(s.rejected(), 1);
    assert_eq!(s.accepted(), 0);
  }

  #[test]
  fn parking_an_input_in_the_future_is_rejected() {
    let mut s = InputSchedule::new();
    assert_eq!(s.submit(100, "parked", 10, WINDOW), Submission::TooFarAhead);
    assert_eq!(s.execute_due(100), None);
  }

  #[test]
  fn late_inside_the_window_executes_next_tick_and_is_counted() {
    // Lands on honest slow links too, which is why it is a counter: a steady
    // stream of these says the window is too tight for who is connected.
    let mut s = InputSchedule::new();
    assert_eq!(s.submit(10, "late", 12, WINDOW), Submission::Late);
    assert_eq!(s.late(), 1);
    assert_eq!(s.execute_due(12), Some("late"), "applied on the next tick to run");
  }

  #[test]
  fn a_client_cannot_reorder_its_own_history() {
    // Having executed on tick 12, a later submission naming tick 10 (still
    // inside the window) must not execute before what already happened.
    let mut s = InputSchedule::new();
    let _ = s.submit(12, "first", 12, WINDOW);
    assert_eq!(s.execute_due(12), Some("first"));
    let _ = s.submit(10, "rewound", 12, WINDOW);
    assert_eq!(s.execute_due(12), Some("rewound"), "clamped to the executed frontier, not earlier");
  }

  #[test]
  fn within_one_step_the_newest_due_input_wins() {
    // A held direction is a level, not an edge: three inputs coming due on the
    // same step resolve to the last intended one.
    let mut s = InputSchedule::new();
    let _ = s.submit(10, "a", 9, WINDOW);
    let _ = s.submit(11, "b", 9, WINDOW);
    let _ = s.submit(12, "c", 9, WINDOW);
    assert_eq!(s.execute_due(12), Some("c"));
    assert_eq!(s.execute_due(13), None, "the superseded ones are gone, not deferred");
  }

  #[test]
  fn clearing_a_vacated_seat_keeps_the_sessions_counters() {
    let mut s = InputSchedule::new();
    let _ = s.submit(10, "x", 10, WINDOW);
    s.clear();
    assert_eq!(s.execute_due(10), None);
    assert_eq!(s.accepted(), 1, "counters describe the session, not the occupant");
  }
}
