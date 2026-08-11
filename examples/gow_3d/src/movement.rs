//! Who decides where you are.
//!
//! Every other example in this tree is server-authoritative, because that is
//! the right default and plaza is built for it. This genre is not: the client
//! says where it is and the server sanity-checks. It buys perfectly smooth
//! local movement with no prediction, no reconciliation and no correction to
//! ease off, and it costs a class of cheating that cannot be closed, only
//! bounded.
//!
//! **The honest part is what a validator cannot do.** It can catch a player
//! crossing a zone in a second. It cannot catch one moving ten percent faster
//! than they should, because a ten percent overrun is indistinguishable from a
//! late packet, and a threshold tight enough to catch it throws out honest
//! players on bad connections. Anyone reading this as an endorsement has been
//! failed by the example: it is a demonstration of a trade with the price
//! visible.

/// Units a character may cover in a second, honestly.
pub const RUN_SPEED: f32 = 7.0;

/// How much over that a validator tolerates before refusing a step.
///
/// Slack for a late packet, a frame that ran long, or a clock that drifted.
/// Every bit of it is also room a cheat can hide in, which is the trade stated
/// as a constant.
pub const TOLERANCE: f32 = 1.35;

/// What the server did with a claimed position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
  /// Plausible, and taken as the truth.
  Accepted,
  /// Too far for the time elapsed. The server keeps where it had them.
  Refused,
}

/// A character the server is tracking but not simulating.
#[derive(Clone, Copy, Debug)]
pub struct Tracked {
  pub at: (f32, f32, f32),
  /// When the last accepted claim was made, in milliseconds.
  pub at_ms: u64,
  pub refusals: u32,
}

impl Tracked {
  pub fn new(at: (f32, f32, f32), now_ms: u64) -> Self {
    Self {
      at,
      at_ms: now_ms,
      refusals: 0,
    }
  }

  /// Takes a claimed position, if it is one the character could have reached.
  ///
  /// The elapsed time is the server's, not the client's, or the cheat is to
  /// claim a long gap and a long distance together.
  pub fn claim(&mut self, to: (f32, f32, f32), now_ms: u64) -> Verdict {
    let elapsed = now_ms.saturating_sub(self.at_ms) as f32 / 1000.0;
    let allowed = RUN_SPEED * TOLERANCE * elapsed;
    let moved = distance(self.at, to);
    // A first claim after a long silence is allowed a long distance, which is
    // correct: the character really did have that long to walk.
    if moved > allowed {
      self.refusals += 1;
      return Verdict::Refused;
    }
    self.at = to;
    self.at_ms = now_ms;
    Verdict::Accepted
  }
}

pub fn distance(a: (f32, f32, f32), b: (f32, f32, f32)) -> f32 {
  let (dx, dy, dz) = (b.0 - a.0, b.1 - a.1, b.2 - a.2);
  (dx * dx + dy * dy + dz * dz).sqrt()
}

/// How far a character gets in a second at a given multiple of the honest
/// speed, once a validator has refused everything it can.
///
/// The number that prices the trade: not whether a cheat is possible, but how
/// much of one survives.
pub fn gained(multiplier: f32, ticks: u32, step_ms: u64) -> f32 {
  let mut tracked = Tracked::new((0.0, 0.0, 0.0), 0);
  let step = step_ms as f32 / 1000.0;
  let mut wanted = 0.0f32;
  for tick in 1..=ticks {
    let now = tick as u64 * step_ms;
    wanted += RUN_SPEED * multiplier * step;
    tracked.claim((wanted, 0.0, 0.0), now);
  }
  tracked.at.0
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_plausible_step_is_taken_as_the_truth() {
    // Which is the whole point of the mode: no prediction, no reconciliation,
    // and nothing to ease off, because the client was right by definition.
    let mut t = Tracked::new((0.0, 0.0, 0.0), 0);
    assert_eq!(t.claim((0.1, 0.0, 0.0), 50), Verdict::Accepted);
    assert_eq!(t.at.0, 0.1);
    assert_eq!(t.refusals, 0);
  }

  #[test]
  fn crossing_the_zone_in_a_frame_is_refused_and_the_server_keeps_its_own() {
    let mut t = Tracked::new((0.0, 0.0, 0.0), 0);
    assert_eq!(t.claim((400.0, 0.0, 0.0), 16), Verdict::Refused);
    assert_eq!(t.at, (0.0, 0.0, 0.0), "refused means the server keeps where it had them");
    assert_eq!(t.refusals, 1);
  }

  #[test]
  fn a_long_silence_earns_a_long_step() {
    // A character really did have ten seconds to walk, so refusing the distance
    // would be punishing a client for a gap the network caused.
    let mut t = Tracked::new((0.0, 0.0, 0.0), 0);
    assert_eq!(t.claim((60.0, 0.0, 0.0), 10_000), Verdict::Accepted);
  }

  #[test]
  fn the_elapsed_time_is_the_servers_or_the_cheat_is_to_claim_both() {
    // Nothing in `claim` reads a client clock, and this is what that buys: a
    // client cannot pair a long distance with a long gap of its own invention.
    let mut t = Tracked::new((0.0, 0.0, 0.0), 0);
    assert_eq!(t.claim((60.0, 0.0, 0.0), 16), Verdict::Refused);
  }

  #[test]
  fn what_a_validator_actually_stops() {
    // The honest table. A tolerance wide enough for a late packet is wide
    // enough for a cheat that size, and no threshold separates them, because
    // they are the same observation.
    println!("\n  a second of running, against an honest 7.0 units:\n");
    println!("{:>12} {:>12} {:>10}", "claimed", "achieved", "gain");
    let honest = gained(1.0, 60, 16);
    for multiplier in [1.0f32, 1.1, 1.3, 2.0, 10.0] {
      let got = gained(multiplier, 60, 16);
      println!("{:>11.1}x {got:>12.2} {:>9.2}x", multiplier, got / honest);
    }
    println!("\n  everything up to the tolerance is kept, because a ten percent\n  overrun and a late packet are the same observation.\n");

    // Under the tolerance, a cheat is simply allowed.
    assert!(gained(1.3, 60, 16) > honest * 1.25, "a cheat inside the slack is a cheat that works");
    // Far past it, almost nothing survives: the server keeps its own position
    // every time, so the character barely advances.
    assert!(gained(10.0, 60, 16) < honest * 1.4, "and one past it gains nearly nothing");
  }

  #[test]
  fn a_refusal_count_is_the_only_signal_there_is() {
    // Which is why the panel shows it. A single refusal is a bad frame; a
    // thousand is a client that is not playing the same game, and telling
    // those apart is a judgement rather than a rule.
    let mut honest = Tracked::new((0.0, 0.0, 0.0), 0);
    let mut cheat = Tracked::new((0.0, 0.0, 0.0), 0);
    for tick in 1..=60u64 {
      let now = tick * 16;
      let step = RUN_SPEED * 0.016;
      honest.claim((tick as f32 * step, 0.0, 0.0), now);
      cheat.claim((tick as f32 * step * 10.0, 0.0, 0.0), now);
    }
    assert_eq!(honest.refusals, 0);
    assert!(cheat.refusals > 50, "a blatant one is loud: {}", cheat.refusals);
  }
}
