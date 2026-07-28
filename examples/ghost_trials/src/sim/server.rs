//! The authority, which never watches anybody race.
//!
//! It holds the track, the leaderboard, and the rules version. What it does not
//! hold is a simulation of the current runs: a time trial has nothing to
//! arbitrate between players, and the thing that actually needs deciding, "is
//! this time real", is decided **after** the fact by reconstruction.
//!
//! That is a different shape of authority from every other example here, and it
//! is the shape event sourcing buys you. The server is not a referee watching
//! the match. It is a court that replays the evidence.

use crate::sim::log::{self, InputLog, Rejection};
use crate::sim::protocol::{Ghost, Op, PROTOCOL};
use crate::sim::types::*;

/// How many ghosts to keep and hand out. A leaderboard, not an archive.
pub const KEPT_GHOSTS: usize = 6;

#[derive(Clone, Debug)]
pub struct Server {
  pub track: Track,
  pub rules_version: u32,
  /// Best runs, fastest first.
  pub board: Vec<Ghost>,
  seats: Vec<bool>,
  next_ghost: u32,
  clock_ms: u64,

  pub submissions: u64,
  pub accepted: u64,
  pub refused: u64,
  pub last_refusal: Option<Rejection>,
  /// Ticks replayed while verifying, so the cost of checking by reconstruction
  /// is a number rather than a worry.
  pub ticks_replayed: u64,
  pub bytes_in: u64,
  pub bytes_out: u64,
  /// What the same ghosts would have cost as sampled paths.
  pub bytes_if_paths: u64,
}

impl Server {
  pub fn new(seats: usize) -> Self {
    Self {
      track: Track::circuit(),
      rules_version: PROTOCOL,
      board: Vec::new(),
      seats: vec![false; seats.clamp(1, 4)],
      next_ghost: 1,
      clock_ms: 0,
      submissions: 0,
      accepted: 0,
      refused: 0,
      last_refusal: None,
      ticks_replayed: 0,
      bytes_in: 0,
      bytes_out: 0,
      bytes_if_paths: 0,
    }
  }

  pub fn seats(&self) -> usize {
    self.seats.len()
  }

  pub fn now_ms(&self) -> u64 {
    self.clock_ms
  }

  pub fn advance(&mut self, dt_ms: u64) {
    self.clock_ms += dt_ms;
  }

  pub fn take_seat(&mut self, seat: usize) -> bool {
    match self.seats.get_mut(seat) {
      Some(taken) if !*taken => {
        *taken = true;
        true
      }
      _ => false,
    }
  }

  pub fn release_seat(&mut self, seat: usize) {
    if let Some(taken) = self.seats.get_mut(seat) {
      *taken = false;
    }
  }

  pub fn welcome(&self, seat: usize) -> Op {
    Op::Welcome {
      player: seat as PlayerId,
      protocol: self.rules_version,
      track: Box::new(self.track.clone()),
      ghosts: self.board.clone(),
      server_time_ms: self.clock_ms,
    }
  }

  /// The best time on the board, if there is one.
  pub fn record(&self) -> Option<u64> {
    self.board.first().map(|g| g.time_ms)
  }

  /// Takes a submission, and decides it by replaying it.
  ///
  /// The claimed time is compared, never adopted. A client that sends a time
  /// its own log does not produce is either broken or lying, and the two look
  /// identical from here, so both get the same answer.
  pub fn submit(&mut self, seat: usize, log: InputLog, claimed_ms: u64) -> Vec<Op> {
    self.submissions += 1;
    self.bytes_in += 12 + log.wire_cost() as u64;
    self.ticks_replayed += log.ticks().min(log::MAX_TICKS) as u64;

    match log::verify(&log, claimed_ms, &self.track, self.rules_version) {
      Err(why) => {
        self.refused += 1;
        self.last_refusal = Some(why);
        vec![Op::Refused { why }]
      }
      Ok(replay) => {
        self.accepted += 1;
        let time_ms = replay.time_ms().expect("verified");
        let ghost = Ghost {
          id: self.next_ghost,
          player: seat as PlayerId,
          time_ms,
          log,
        };
        self.next_ghost += 1;

        self.board.push(ghost.clone());
        self.board.sort_by_key(|g| (g.time_ms, g.id));
        self.board.truncate(KEPT_GHOSTS);
        let place = self.board.iter().position(|g| g.id == ghost.id).map(|i| i as u32 + 1).unwrap_or(0);

        self.bytes_out += ghost.wire_cost() as u64 * self.seats.len() as u64;
        self.bytes_if_paths += (16 + ghost.log.path_cost()) as u64 * self.seats.len() as u64;
        vec![Op::Accepted {
          ghost: Box::new(ghost),
          place,
        }]
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::sim::log::Recorder;
  use crate::sim::rules;

  /// A lap driven by aiming at the next ring, the same fixture the log tests
  /// use, so the two files agree about what a run looks like.
  fn a_run(version: u32) -> (InputLog, u64) {
    let track = Track::circuit();
    let mut racer = Racer::at_start(&track);
    let mut recorder = Recorder::new(version);
    let mut finished = None;
    for tick in 0..log::MAX_TICKS {
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
      let input = Input::new(steer, tick % 200 < 40);
      recorder.observe(input);
      rules::step(&mut racer, input, &track);
      if rules::finished(&racer) {
        finished = Some(tick);
        break;
      }
    }
    let log = recorder.finish();
    // One more than the index: the tick it finished *on* is a tick it took.
    (log, (finished.expect("the fixture finishes") as u64 + 1) * SIM_STEP_MS)
  }

  #[test]
  fn an_honest_run_is_accepted_and_becomes_a_ghost() {
    let mut server = Server::new(2);
    let (log, time) = a_run(server.rules_version);
    let out = server.submit(0, log, time);

    match out.as_slice() {
      [Op::Accepted { ghost, place }] => {
        assert_eq!(ghost.time_ms, time, "the time on the board is the replayed one");
        assert_eq!(*place, 1, "and it is the only one there");
      }
      other => panic!("{other:?}"),
    }
    assert_eq!(server.record(), Some(time));
  }

  #[test]
  fn a_faked_time_is_refused_and_never_reaches_the_board() {
    // The claim is a checksum on the evidence, not the evidence.
    let mut server = Server::new(2);
    let (log, time) = a_run(server.rules_version);
    let out = server.submit(0, log, time / 3);

    assert!(matches!(out.as_slice(), [Op::Refused { .. }]));
    assert_eq!(server.board.len(), 0, "nothing was recorded");
    assert_eq!(server.refused, 1);
  }

  #[test]
  fn a_log_from_other_rules_is_refused_by_version_rather_than_by_replay() {
    // Refused *before* being replayed. Replaying it would produce some run, and
    // that run would be a lie about what its player drove.
    let mut server = Server::new(1);
    let (mut log, time) = a_run(server.rules_version);
    log.rules_version = server.rules_version.wrapping_add(1);
    let out = server.submit(0, log, time);

    assert!(matches!(
      out.as_slice(),
      [Op::Refused {
        why: Rejection::WrongRules { .. }
      }]
    ));
  }

  #[test]
  fn the_board_keeps_the_fastest_and_forgets_the_rest() {
    let mut server = Server::new(1);
    let (log, time) = a_run(server.rules_version);
    for _ in 0..(KEPT_GHOSTS + 3) {
      server.submit(0, log.clone(), time);
    }
    assert_eq!(server.board.len(), KEPT_GHOSTS, "a leaderboard, not an archive");
    assert!(server.board.windows(2).all(|w| w[0].time_ms <= w[1].time_ms), "fastest first");
  }

  #[test]
  fn a_ghost_costs_a_fraction_of_the_path_it_replaces() {
    let mut server = Server::new(2);
    let (log, time) = a_run(server.rules_version);
    server.submit(0, log, time);
    assert!(
      server.bytes_out * 8 < server.bytes_if_paths,
      "{} bytes of ghost against {} of path",
      server.bytes_out,
      server.bytes_if_paths
    );
  }

  #[test]
  fn verifying_by_replay_costs_one_run_of_the_rules() {
    // Worth having as a number: "the server replays every submission" sounds
    // expensive until you notice it is a couple of thousand ticks of integer
    // maths, once, at the end of a run somebody spent thirty seconds driving.
    let mut server = Server::new(1);
    let (log, time) = a_run(server.rules_version);
    let ticks = log.ticks() as u64;
    server.submit(0, log, time);
    assert_eq!(server.ticks_replayed, ticks);
    assert!(ticks < 3_000, "one trial is {ticks} ticks");
  }
}
