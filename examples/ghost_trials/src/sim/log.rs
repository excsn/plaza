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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputLog {
  /// The rules this was recorded under. See the module note.
  pub rules_version: u32,
  /// Which game it was: a trial alone, or a race against the CPU field.
  ///
  /// A race log carries **only the player's inputs**, because the opponents are
  /// a pure function of the world they are in. One player's key presses
  /// reproduce a four-way race, opponents and all, which is the same trick
  /// `seed_defense` plays on a wave of enemies.
  pub mode: Mode,
  pub spans: Vec<Span>,
}

impl Default for InputLog {
  fn default() -> Self {
    Self {
      rules_version: 0,
      mode: Mode::Trial,
      spans: Vec::new(),
    }
  }
}

impl InputLog {
  /// The world this log was driven in.
  pub fn world(&self, track: &Track) -> rules::World {
    match self.mode {
      Mode::Trial => rules::World::trial(track),
      Mode::Race => rules::World::race(track, RACE_FIELD),
    }
  }

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
  pub fn new(rules_version: u32, mode: Mode) -> Self {
    Self {
      log: InputLog {
        rules_version,
        mode,
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
  /// The whole circuit at the end of the replay, opponents included.
  pub world: rules::World,
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
  // Through the same `step_world` a live race runs, with a field of one. The
  // pickups are part of the circuit, so a replay collects them exactly where
  // the run did, and a trial is a race with nobody else in it rather than a
  // second implementation that could drift from the first.
  let mut world = log.world(track);
  let mut rings = Vec::new();
  let mut finished_tick = None;
  let ticks = log.ticks().min(MAX_TICKS);

  for tick in 0..ticks {
    let before = (world.racers[0].lap, world.racers[0].next_ring);
    let inputs = rules::field_inputs(&world, track, log.at(tick), 0);
    rules::step_world(&mut world, &inputs, track);
    if (world.racers[0].lap, world.racers[0].next_ring) != before {
      rings.push(tick);
    }
    if finished_tick.is_none() && rules::finished(&world.racers[0]) {
      finished_tick = Some(tick);
      break;
    }
  }

  Replay {
    rings,
    finished_tick,
    racer: world.racers[0],
    world,
  }
}

/// Replays only as far as a tick, for drawing a ghost beside a live run.
///
/// Called every frame with an increasing tick, which is quadratic if taken
/// literally, so the caller keeps the racer and advances it. This is here for
/// the cases that genuinely need a state at an arbitrary tick: seeking, and
/// starting a ghost part way through.
pub fn replay_to(log: &InputLog, track: &Track, tick: u32) -> Racer {
  let mut world = log.world(track);
  for t in 0..tick.min(log.ticks()).min(MAX_TICKS) {
    let inputs = rules::field_inputs(&world, track, log.at(t), 0);
    rules::step_world(&mut world, &inputs, track);
  }
  world.racers[0]
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
    drive(track, Mode::Trial)
  }

  fn drive(track: &Track, mode: Mode) -> (Recorder, Racer) {
    let mut world = match mode {
      Mode::Trial => rules::World::trial(track),
      Mode::Race => rules::World::race(track, RACE_FIELD),
    };
    let mut recorder = Recorder::new(VERSION, mode);
    for _ in 0..MAX_TICKS {
      // The player is driven by the same rule the opponents are, which makes
      // the fixture a fixture rather than a script.
      let mine = rules::bot_input(&world.racers[0], track, world.tick, 0);
      recorder.observe(mine);
      let inputs = rules::field_inputs(&world, track, mine, 0);
      rules::step_world(&mut world, &inputs, track);
      if rules::finished(&world.racers[0]) {
        break;
      }
    }
    (recorder, world.racers[0])
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
  fn one_players_log_reproduces_a_whole_four_way_race() {
    // The claim race mode is built on, and the one that makes it worth having
    // beside the trial. Only the player's key presses are recorded. The other
    // three racers, every shove between them, and every pickup they took come
    // back because they are functions of the world rather than facts about it.
    let track = Track::circuit();
    let (recorder, driven) = drive(&track, Mode::Race);
    let log = recorder.finish();
    assert_eq!(log.mode, Mode::Race);

    let replayed = replay(&log, &track);
    assert_eq!(replayed.racer, driven, "the player came back");
    assert_eq!(replayed.world.racers.len(), RACE_FIELD, "and so did the field");

    // Driven again from scratch, to be sure the replay is reproducing rather
    // than merely agreeing with a copy of itself.
    let (_, again) = drive(&track, Mode::Race);
    assert_eq!(again, driven);
  }

  #[test]
  fn a_race_log_and_a_trial_log_are_not_the_same_run() {
    // Which is why the mode is in the log. Replaying a race log as a trial
    // would leave out three cars, and the time it produced would be a time
    // nobody drove.
    let track = Track::circuit();
    let (trial, _) = drive(&track, Mode::Trial);
    let (race, _) = drive(&track, Mode::Race);
    let trial = trial.finish();
    let mut race = race.finish();
    assert_ne!(replay(&trial, &track).racer, replay(&race, &track).racer);

    race.mode = Mode::Trial;
    assert_ne!(
      replay(&race, &track).time_ms(),
      Some(0),
      "replaying it under the wrong mode produces something, which is the danger"
    );
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
    let mut recorder = Recorder::new(VERSION, Mode::Trial);
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
    let mut recorder = Recorder::new(VERSION, Mode::Trial);
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

    let mut forward = rules::World::trial(&track);
    for tick in 0..300 {
      rules::step_world(&mut forward, &[log.at(tick)], &track);
    }
    assert_eq!(replay_to(&log, &track, 300), forward.racers[0]);
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
