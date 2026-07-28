//! The client, which owns the whole of the feel and none of the verdict.
//!
//! A time trial has nothing to arbitrate between players, so this client is not
//! predicting anything and is never corrected. It drives, it records, and when
//! it is done it hands over the evidence. The server's answer arrives a round
//! trip later and changes nothing about how the car handled.
//!
//! Two things here are worth more than the driving.
//!
//! **The ghosts are replays, advanced in lockstep with the local tick.** Each
//! one is a racer stepped by the same rule from the same log, so a ghost is not
//! an animation of a recorded path, it is the run happening again.
//!
//! **The self check compares the recording against the run.** When a trial
//! ends, the finished log is replayed and the result compared to the racer that
//! was actually driven. On one machine that should be impossible to fail, which
//! is the point: it does not test the physics, it tests the *recorder*, and a
//! recorder that is off by one tick at a span boundary produces a ghost that
//! drifts away from the run it came from. Nothing else would notice.

use crate::sim::log::{self, InputLog, Recorder, Rejection};
use crate::sim::protocol::Ghost;
use crate::sim::types::*;

/// A ghost being raced against: its log, and where in it we are.
#[derive(Clone, Debug)]
pub struct GhostRun {
  pub ghost: Ghost,
  pub racer: Racer,
  pub tick: u32,
  pub done: bool,
}

pub struct Client {
  pub me: PlayerId,
  pub track: Track,
  pub rules_version: u32,

  pub racer: Racer,
  pub tick: u32,
  pub running: bool,
  /// The time this run took, once it is over.
  pub finished_ms: Option<u64>,
  recorder: Recorder,

  pub ghosts: Vec<GhostRun>,
  pub best_ms: Option<u64>,
  pub last_place: Option<u32>,
  pub last_refusal: Option<Rejection>,
  pub submissions: u64,
  /// Times the finished log did not replay to the run that produced it. Should
  /// be zero for ever; see the module note for why it is counted anyway.
  pub self_check_failures: u64,
  pub self_checks: u64,
  /// A log waiting to be sent, with the time it claims.
  outbox: Option<(InputLog, u64)>,
}

impl Client {
  pub fn new(me: PlayerId, track: Track, rules_version: u32) -> Self {
    let racer = Racer::at_start(&track);
    Self {
      me,
      track,
      rules_version,
      racer,
      tick: 0,
      running: false,
      finished_ms: None,
      recorder: Recorder::new(rules_version),
      ghosts: Vec::new(),
      best_ms: None,
      last_place: None,
      last_refusal: None,
      submissions: 0,
      self_check_failures: 0,
      self_checks: 0,
      outbox: None,
    }
  }

  /// Starts a fresh attempt. Every ghost restarts with it, so they race you
  /// from the line rather than from wherever they happened to be.
  pub fn restart(&mut self) {
    self.racer = Racer::at_start(&self.track);
    self.tick = 0;
    self.running = true;
    self.finished_ms = None;
    self.recorder = Recorder::new(self.rules_version);
    for ghost in self.ghosts.iter_mut() {
      ghost.racer = Racer::at_start(&self.track);
      ghost.tick = 0;
      ghost.done = false;
    }
  }

  pub fn elapsed_ms(&self) -> u64 {
    self.tick as u64 * SIM_STEP_MS
  }

  /// Advances the trial by one tick under one input.
  ///
  /// The input is recorded and applied in the same call, which is the only way
  /// to be sure the log says what the run did. Recording somewhere else, from a
  /// value read somewhere else, is how a ghost and its run come apart.
  pub fn step(&mut self, input: Input, controls: &Controls) {
    if !self.running {
      return;
    }
    self.recorder.observe(input);
    crate::sim::rules::step(&mut self.racer, input, &self.track);
    self.tick += 1;

    for ghost in self.ghosts.iter_mut() {
      if ghost.done {
        continue;
      }
      if ghost.tick >= ghost.ghost.log.ticks() {
        ghost.done = true;
        continue;
      }
      crate::sim::rules::step(&mut ghost.racer, ghost.ghost.log.at(ghost.tick), &self.track);
      ghost.tick += 1;
    }

    if crate::sim::rules::finished(&self.racer) {
      self.finish(controls);
    } else if self.tick >= log::MAX_TICKS {
      self.running = false;
    }
  }

  fn finish(&mut self, controls: &Controls) {
    self.running = false;
    let time = self.elapsed_ms();
    self.finished_ms = Some(time);
    self.best_ms = Some(self.best_ms.map_or(time, |b| b.min(time)));

    let finished = self.recorder.finish();

    if controls.self_check {
      self.self_checks += 1;
      let replayed = log::replay(&finished, &self.track);
      if replayed.racer != self.racer || replayed.time_ms() != Some(time) {
        self.self_check_failures += 1;
      }
    }

    // The claim. Honest by default; the panel can make it a lie, which is the
    // only way to watch the verification do its job.
    let claimed = if controls.cheat { time.saturating_sub(time / 3).max(1) } else { time };
    self.outbox = Some((finished, claimed));
  }

  /// The submission waiting to go out, if any.
  pub fn take_submission(&mut self) -> Option<(InputLog, u64)> {
    let out = self.outbox.take();
    if out.is_some() {
      self.submissions += 1;
    }
    out
  }

  pub fn on_ghosts(&mut self, ghosts: Vec<Ghost>) {
    for ghost in ghosts {
      self.add_ghost(ghost);
    }
  }

  /// Takes a verified run and starts racing it.
  ///
  /// It is caught up to the local tick, so a ghost that arrives mid-attempt
  /// appears where it *would* be rather than at the line. That is the one place
  /// this example needs to reconstruct a state at an arbitrary moment, and it
  /// is one line, because the log can produce any tick on demand.
  pub fn add_ghost(&mut self, ghost: Ghost) {
    if self.ghosts.iter().any(|g| g.ghost.id == ghost.id) {
      return;
    }
    let racer = log::replay_to(&ghost.log, &self.track, self.tick);
    self.ghosts.push(GhostRun {
      racer,
      tick: self.tick.min(ghost.log.ticks()),
      done: false,
      ghost,
    });
    self.ghosts.sort_by_key(|g| (g.ghost.time_ms, g.ghost.id));
  }

  pub fn on_refused(&mut self, why: Rejection) {
    self.last_refusal = Some(why);
  }

  pub fn on_accepted(&mut self, ghost: Ghost, place: u32) {
    if ghost.player == self.me {
      self.last_place = Some(place);
    }
    self.add_ghost(ghost);
  }

  /// The ghost this client is closest to beating, for the split readout.
  pub fn rival(&self) -> Option<&GhostRun> {
    self.ghosts.first()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::sim::server::Server;

  fn controls() -> Controls {
    Controls {
      latency_ms: 0,
      jitter_ms: 0,
      loss_pct: 0.0,
      ..Controls::default()
    }
  }

  /// Drives a client to the flag with the same autopilot the other tests use.
  fn drive(client: &mut Client, controls: &Controls) {
    client.restart();
    while client.running {
      let input = autopilot(&client.racer, &client.track, client.tick);
      client.step(input, controls);
    }
  }

  pub fn autopilot(racer: &Racer, track: &Track, tick: u32) -> Input {
    let target = track.ring(racer.next_ring);
    let want = angle_between(racer.pos, target);
    let delta = (want + BRADS - racer.heading) % BRADS;
    const DEADBAND: u16 = 24;
    let steer = if delta <= DEADBAND || delta >= BRADS - DEADBAND {
      0
    } else if delta < BRADS / 2 {
      1
    } else {
      -1
    };
    Input::new(steer, tick % 200 < 40)
  }

  #[test]
  fn a_finished_run_replays_to_itself() {
    // The self check, which tests the recorder rather than the physics.
    let c = controls();
    let server = Server::new(1);
    let mut client = Client::new(0, server.track.clone(), server.rules_version);
    drive(&mut client, &c);

    assert!(client.finished_ms.is_some(), "it finished");
    assert_eq!(client.self_checks, 1);
    assert_eq!(client.self_check_failures, 0, "the log is the run");
  }

  #[test]
  fn the_submission_carries_the_log_and_the_time_it_produces() {
    let c = controls();
    let server = Server::new(1);
    let mut client = Client::new(0, server.track.clone(), server.rules_version);
    drive(&mut client, &c);

    let (log, claimed) = client.take_submission().expect("something to send");
    assert_eq!(Some(claimed), client.finished_ms);
    assert_eq!(log.rules_version, server.rules_version);
    assert!(client.take_submission().is_none(), "and it is sent once");
  }

  #[test]
  fn a_ghost_arriving_mid_attempt_appears_where_it_would_be() {
    // The one place a state has to be reconstructed at an arbitrary moment,
    // which an event log makes a one-liner.
    let c = controls();
    let mut server = Server::new(1);
    let mut driver = Client::new(0, server.track.clone(), server.rules_version);
    drive(&mut driver, &c);
    let (log, time) = driver.take_submission().expect("a run");
    let ghost = match server.submit(0, log, time).pop() {
      Some(crate::sim::protocol::Op::Accepted { ghost, .. }) => *ghost,
      other => panic!("{other:?}"),
    };

    let mut watcher = Client::new(1, server.track.clone(), server.rules_version);
    watcher.restart();
    for _ in 0..250 {
      watcher.step(Input::default(), &c);
    }
    watcher.add_ghost(ghost.clone());

    let caught_up = &watcher.ghosts[0];
    assert_eq!(caught_up.tick, 250);
    assert_eq!(caught_up.racer, log::replay_to(&ghost.log, &watcher.track, 250));
    assert_ne!(caught_up.racer.pos, Racer::at_start(&watcher.track).pos, "not sitting on the line");
  }

  #[test]
  fn a_ghost_races_the_run_it_was_recorded_from() {
    // Raced tick for tick beside a fresh attempt driven by the same autopilot:
    // the two must be in exactly the same place the whole way round, because
    // they are the same inputs through the same rules.
    let c = controls();
    let mut server = Server::new(1);
    let mut first = Client::new(0, server.track.clone(), server.rules_version);
    drive(&mut first, &c);
    let (log, time) = first.take_submission().expect("a run");
    let ghost = match server.submit(0, log, time).pop() {
      Some(crate::sim::protocol::Op::Accepted { ghost, .. }) => *ghost,
      other => panic!("{other:?}"),
    };

    let mut second = Client::new(1, server.track.clone(), server.rules_version);
    second.add_ghost(ghost);
    second.restart();
    while second.running {
      let input = autopilot(&second.racer, &second.track, second.tick);
      second.step(input, &c);
      let against = &second.ghosts[0];
      if !against.done {
        assert_eq!(against.racer, second.racer, "the ghost diverged at tick {}", second.tick);
      }
    }
    assert_eq!(second.finished_ms, first.finished_ms);
  }

  #[test]
  fn the_cheat_switch_sends_a_time_the_log_does_not_support() {
    let c = Controls { cheat: true, ..controls() };
    let server = Server::new(1);
    let mut client = Client::new(0, server.track.clone(), server.rules_version);
    drive(&mut client, &c);

    let (_, claimed) = client.take_submission().expect("a submission");
    assert!(claimed < client.finished_ms.expect("finished"), "it claims better than it drove");
  }
}
