//! The op log: a run, stored as the inputs that produced it.
//!
//! This is the module the example exists for. `plaza`'s op stream is an
//! event-sourced record, which means state is not the thing you keep, it is the
//! thing you can always get back. Nothing else in this repository takes that
//! literally. Here it is the entire design:
//!
//! - **A ghost is a log, not a path.** Replaying the inputs through the shared
//!   rule reproduces the run exactly, so a ghost costs its *inputs* rather than
//!   its positions. A two-lap run at 50 Hz is a couple of thousand ticks and a
//!   few hundred bytes, against about twelve kilobytes of sampled positions.
//! - **A time is not a claim, it is a consequence.** The server does not watch
//!   anybody race. It is handed a log, replays it, and reads the time off the
//!   replay. A client can send any number it likes; the number it sends is not
//!   what gets recorded.
//! - **A log is only as good as the rules it was recorded under.** Replay is
//!   reproduction, and reproduction is a bet that today's arithmetic matches
//!   the arithmetic that recorded it. So a log carries the version of the rules
//!   it was made under, and one from a different version is refused rather than
//!   replayed wrong.
//!
//! The encoding is the op stream's own shape: an entry per *change* of input,
//! not per tick. That is not a compression trick applied afterwards, it is what
//! an event log already looks like.

use serde::{Deserialize, Serialize};

use crate::sim::rules;
use crate::sim::types::*;

/// One held input, and the tick it stops being held on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
  pub until_tick: u32,
  pub input: Input,
}

/// A whole run, as the inputs that produced it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputLog {
  /// The rules this was recorded under. See the module note.
  pub rules_version: u32,
  pub spans: Vec<Span>,
}

impl InputLog {
  /// How many ticks the log covers.
  pub fn ticks(&self) -> u32 {
    self.spans.last().map(|s| s.until_tick).unwrap_or(0)
  }

  /// The input held on a tick.
  pub fn at(&self, tick: u32) -> Input {
    match self.spans.iter().find(|s| tick < s.until_tick) {
      Some(span) => span.input,
      None => Input::default(),
    }
  }

  /// Roughly what this costs on the wire.
  pub fn wire_cost(&self) -> usize {
    8 + self.spans.len() * 5
  }

  /// What the same run would have cost as a stream of positions, at one sample
  /// per tick: two coordinates and a heading.
  ///
  /// The comparison the panel shows, and the reason to keep the inputs instead.
  pub fn path_cost(&self) -> usize {
    8 + self.ticks() as usize * 10
  }
}

/// Turns a live run into a log, one entry per change of input.
#[derive(Clone, Debug, Default)]
pub struct Recorder {
  log: InputLog,
  held: Input,
  tick: u32,
  started: bool,
}

impl Recorder {
  pub fn new(rules_version: u32) -> Self {
    Self {
      log: InputLog {
        rules_version,
        spans: Vec::new(),
      },
      held: Input::default(),
      tick: 0,
      started: false,
    }
  }

  /// Records the input for one tick. Call once per simulation step, with the
  /// same input that step was given.
  pub fn observe(&mut self, input: Input) {
    if !self.started {
      self.held = input;
      self.started = true;
    } else if input != self.held {
      self.log.spans.push(Span {
        until_tick: self.tick,
        input: self.held,
      });
      self.held = input;
    }
    self.tick += 1;
  }

  /// The finished log. Closes the span that was still being held.
  pub fn finish(&self) -> InputLog {
    let mut log = self.log.clone();
    if self.started {
      log.spans.push(Span {
        until_tick: self.tick,
        input: self.held,
      });
    }
    log
  }

  pub fn ticks(&self) -> u32 {
    self.tick
  }
}

/// What replaying a log produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replay {
  /// The tick each ring was taken on, in order, across every lap.
  pub rings: Vec<u32>,
  /// The tick the last lap completed on, if it did.
  pub finished_tick: Option<u32>,
  pub racer: Racer,
}

impl Replay {
  /// How long the run took.
  ///
  /// `finished_tick` is the *index* of the tick the last lap completed on, so
  /// the number of ticks taken is one more than it. Getting this wrong is a
  /// twenty millisecond disagreement between the client's clock and the
  /// server's replay, which is invisible until it is a refused submission, and
  /// it is exactly what the client's self check caught the first time it ran.
  pub fn time_ms(&self) -> Option<u64> {
    self.finished_tick.map(|t| (t as u64 + 1) * SIM_STEP_MS)
  }
}

/// Why a log was not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rejection {
  /// Recorded under different rules, so replaying it would produce a different
  /// run from the one the player drove. Refused rather than silently rerun.
  WrongRules { theirs: u32, ours: u32 },
  /// The inputs never complete the trial.
  NeverFinished,
  /// The claimed time is not the time the inputs produce.
  TimeDoesNotMatch { claimed: u64, replayed: u64 },
  /// Longer than any honest run, so replaying it is not worth the cycles.
  TooLong,
}

/// The longest log worth replaying, in ticks. Two minutes.
pub const MAX_TICKS: u32 = 6_000;

/// Runs a log through the shared rule and reports what happened.
///
/// The same function on the client, where it draws a ghost, and on the server,
/// where it decides whether a time is real. One implementation, because two
/// would make a ghost that drives differently from the run it came from.
pub fn replay(log: &InputLog, track: &Track) -> Replay {
  let mut racer = Racer::at_start(track);
  let mut rings = Vec::new();
  let mut finished_tick = None;
  let ticks = log.ticks().min(MAX_TICKS);

  for tick in 0..ticks {
    let before = (racer.lap, racer.next_ring);
    rules::step(&mut racer, log.at(tick), track);
    if (racer.lap, racer.next_ring) != before {
      rings.push(tick);
    }
    if finished_tick.is_none() && rules::finished(&racer) {
      finished_tick = Some(tick);
      break;
    }
  }

  Replay {
    rings,
    finished_tick,
    racer,
  }
}

/// Replays only as far as a tick, for drawing a ghost beside a live run.
///
/// Called every frame with an increasing tick, which is quadratic if taken
/// literally, so the caller keeps the racer and advances it. This is here for
/// the cases that genuinely need a state at an arbitrary tick: seeking, and
/// starting a ghost part way through.
pub fn replay_to(log: &InputLog, track: &Track, tick: u32) -> Racer {
  let mut racer = Racer::at_start(track);
  for t in 0..tick.min(log.ticks()).min(MAX_TICKS) {
    rules::step(&mut racer, log.at(t), track);
  }
  racer
}

/// Checks a submitted log and the time it claims.
///
/// The whole of the anti-cheat, and it is not a heuristic: the log either
/// produces that time or it does not.
pub fn verify(log: &InputLog, claimed_ms: u64, track: &Track, rules_version: u32) -> Result<Replay, Rejection> {
  if log.rules_version != rules_version {
    return Err(Rejection::WrongRules {
      theirs: log.rules_version,
      ours: rules_version,
    });
  }
  if log.ticks() > MAX_TICKS {
    return Err(Rejection::TooLong);
  }
  let replay = replay(log, track);
  let Some(time) = replay.time_ms() else {
    return Err(Rejection::NeverFinished);
  };
  if time != claimed_ms {
    return Err(Rejection::TimeDoesNotMatch {
      claimed: claimed_ms,
      replayed: time,
    });
  }
  Ok(replay)
}

#[cfg(test)]
mod tests {
  use super::*;

  const VERSION: u32 = 7;

  /// Drives a lap or two by aiming at the next ring every tick, which is what a
  /// competent player approximates and what gives a log with real structure in
  /// it rather than a single held input.
  fn drive_a_trial(track: &Track) -> (Recorder, Racer) {
    let mut racer = Racer::at_start(track);
    let mut recorder = Recorder::new(VERSION);
    for tick in 0..MAX_TICKS {
      let input = aim_at_next_ring(&racer, track, tick);
      recorder.observe(input);
      rules::step(&mut racer, input, track);
      if rules::finished(&racer) {
        break;
      }
    }
    (recorder, racer)
  }

  /// Steers toward the ring the racer is looking for, and charges on the run in
  /// to a corner. Deterministic, so the fixture is a fixture.
  fn aim_at_next_ring(racer: &Racer, track: &Track, tick: u32) -> Input {
    let target = track.ring(racer.next_ring);
    let want = angle_between(racer.pos, target);
    let delta = (want + BRADS - racer.heading) % BRADS;
    // A deadband, because without one the fixture flips its steering every
    // tick and the log gets one entry per tick. That is not a bug in the
    // encoding, it is the honest shape of it: an event log is small exactly to
    // the degree that the input holds still, and a bang-bang autopilot is the
    // worst case rather than the typical one.
    const DEADBAND: u16 = 24;
    let steer = if delta <= DEADBAND || delta >= BRADS - DEADBAND {
      0
    } else if delta < BRADS / 2 {
      1
    } else {
      -1
    };
    let charge = tick % 200 < 40;
    Input::new(steer, charge)
  }

  #[test]
  fn a_log_replays_to_the_run_that_produced_it() {
    // The property everything else here rests on. Not "close to": equal.
    let track = Track::circuit();
    let (recorder, driven) = drive_a_trial(&track);
    let log = recorder.finish();
    let replayed = replay(&log, &track);

    assert!(replayed.finished_tick.is_some(), "the fixture finished the trial");
    assert_eq!(replayed.racer, driven, "the replay is the run, exactly");
  }

  #[test]
  fn the_log_is_a_fraction_of_the_path_it_describes() {
    // The measurement, from the same counters the panel shows.
    let track = Track::circuit();
    let (recorder, _) = drive_a_trial(&track);
    let log = recorder.finish();
    assert!(log.ticks() > 400, "a real run: {} ticks", log.ticks());
    // Measured: 146 entries over 1208 ticks, 738 bytes against 12088. The
    // threshold sits well under that because the ratio is a property of how
    // often the *input* changes, and a fixture that steered more would score
    // worse. See the deadband note above for the worst case.
    assert!(
      log.wire_cost() * 8 < log.path_cost(),
      "{} bytes of inputs against {} of positions",
      log.wire_cost(),
      log.path_cost()
    );
  }

  #[test]
  fn an_entry_is_a_change_of_input_rather_than_a_tick() {
    // The encoding is the op stream's own shape. A held key is one entry
    // however long it is held, which is why the log is small.
    let mut recorder = Recorder::new(VERSION);
    for _ in 0..500 {
      recorder.observe(Input::new(1, false));
    }
    for _ in 0..500 {
      recorder.observe(Input::new(0, true));
    }
    let log = recorder.finish();
    assert_eq!(log.spans.len(), 2, "a thousand ticks, two entries");
    assert_eq!(log.ticks(), 1000);
    assert_eq!(log.at(0), Input::new(1, false));
    assert_eq!(log.at(499), Input::new(1, false));
    assert_eq!(log.at(500), Input::new(0, true));
    assert_eq!(log.at(999), Input::new(0, true));
  }

  #[test]
  fn a_faked_time_is_refused_because_the_log_does_not_produce_it() {
    // Not a heuristic and not a plausibility check. The claim is reconstructed
    // and compared, so a client can send any number it likes and the number it
    // sends is not what gets recorded.
    let track = Track::circuit();
    let (recorder, _) = drive_a_trial(&track);
    let log = recorder.finish();
    let real = replay(&log, &track).time_ms().expect("finished");

    assert!(verify(&log, real, &track, VERSION).is_ok(), "the honest time is accepted");
    let err = verify(&log, real / 2, &track, VERSION).expect_err("a halved time is not");
    assert_eq!(
      err,
      Rejection::TimeDoesNotMatch {
        claimed: real / 2,
        replayed: real
      }
    );
  }

  #[test]
  fn a_log_that_never_finishes_is_refused() {
    let track = Track::circuit();
    let mut recorder = Recorder::new(VERSION);
    for _ in 0..600 {
      recorder.observe(Input::new(1, false));
    }
    let log = recorder.finish();
    assert_eq!(
      verify(&log, 12_000, &track, VERSION),
      Err(Rejection::NeverFinished),
      "driving in circles is not a lap"
    );
  }

  #[test]
  fn a_log_from_different_rules_is_refused_rather_than_replayed_wrong() {
    // The failure this example is really about. The log is fine, the player was
    // honest, and the arithmetic that would replay it is not the arithmetic
    // that recorded it. Replaying it anyway would produce a ghost that drives
    // into walls, and a time nobody drove.
    let track = Track::circuit();
    let (recorder, _) = drive_a_trial(&track);
    let mut log = recorder.finish();
    let real = replay(&log, &track).time_ms().expect("finished");
    log.rules_version = VERSION + 1;

    assert_eq!(
      verify(&log, real, &track, VERSION),
      Err(Rejection::WrongRules {
        theirs: VERSION + 1,
        ours: VERSION
      })
    );
  }

  #[test]
  fn seeking_into_a_log_lands_where_playing_it_forward_does() {
    // A ghost has to be able to start part way through, so the two ways of
    // getting to a tick have to agree.
    let track = Track::circuit();
    let (recorder, _) = drive_a_trial(&track);
    let log = recorder.finish();

    let mut forward = Racer::at_start(&track);
    for tick in 0..300 {
      rules::step(&mut forward, log.at(tick), &track);
    }
    assert_eq!(replay_to(&log, &track, 300), forward);
  }

  #[test]
  fn a_log_survives_the_wire_unchanged() {
    // It is the only thing that crosses, so a round trip that lost a tick would
    // lose the run.
    let track = Track::circuit();
    let (recorder, _) = drive_a_trial(&track);
    let log = recorder.finish();

    let bytes = rmp_serde::to_vec_named(&log).expect("encode");
    let back: InputLog = rmp_serde::from_slice(&bytes).expect("decode");
    assert_eq!(back, log);
    assert_eq!(replay(&back, &track).racer, replay(&log, &track).racer);
  }
}
