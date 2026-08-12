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

/// The finest interval the server's clock actually resolves, in milliseconds.
///
/// One tick. A validator cannot tell two claims within a tick apart in time,
/// so it must not pretend they took zero.
pub const CLOCK_GRAIN_MS: u64 = 1000 / 30;

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
  /// When the clock was last read, in milliseconds.
  pub at_ms: u64,
  /// Distance this character has earned and not yet spent.
  ///
  /// A budget rather than a per-claim allowance, and the difference is the
  /// whole of the validator's correctness. Measuring each claim against the
  /// time since the last one has to credit *something* when two arrive in the
  /// same millisecond, and whatever it credits is a rate a client can claim at
  /// will: crediting one tick let a client sending twice a tick move at twice
  /// the speed. A budget cannot be gamed that way because it accrues from the
  /// clock alone, however often it is asked.
  pub budget: f32,
  pub refusals: u32,
}

/// The most distance a character may bank while nobody is hearing from them.
///
/// Uncapped, a disconnection is a teleport: five minutes of silence earns the
/// width of the zone several times over. Capped too tightly, an honest client
/// coming back from a stall is refused for the gap. A few seconds is the
/// compromise, and it is a compromise rather than a solution.
pub const MAX_BANKED_MS: f32 = 3000.0;

impl Tracked {
  pub fn new(at: (f32, f32, f32), now_ms: u64) -> Self {
    Self {
      at,
      at_ms: now_ms,
      // One tick's worth to begin with, or the first claim after being seated
      // is refused for arriving before the clock moved.
      budget: RUN_SPEED * TOLERANCE * (CLOCK_GRAIN_MS as f32 / 1000.0),
      refusals: 0,
    }
  }

  /// Takes a claimed position, if it is one the character could have paid for.
  ///
  /// The elapsed time is the server's, not the client's, or the cheat is to
  /// claim a long gap and a long distance together.
  pub fn claim(&mut self, to: (f32, f32, f32), now_ms: u64) -> Verdict {
    let elapsed = now_ms.saturating_sub(self.at_ms) as f32 / 1000.0;
    self.at_ms = now_ms;
    self.budget = (self.budget + RUN_SPEED * TOLERANCE * elapsed)
      .min(RUN_SPEED * TOLERANCE * (MAX_BANKED_MS / 1000.0));

    // Horizontal only. A run speed is a speed over the ground, and charging a
    // climb against it means walking up a hill is indistinguishable from
    // running, so an honest player on a slope is refused. What a claim does
    // vertically is the air rule's business, and that rule is exact because
    // the ground is derived rather than sent.
    let moved = ground_distance(self.at, to);
    if moved > self.budget {
      self.refusals += 1;
      return Verdict::Refused;
    }
    // Spent rather than reset, so a character who walks slowly banks the rest
    // and one who sprints in bursts averages the same as one who does not.
    self.budget -= moved;
    self.at = to;
    Verdict::Accepted
  }
}

/// Distance over the ground, ignoring the climb.
pub fn ground_distance(a: (f32, f32, f32), b: (f32, f32, f32)) -> f32 {
  let (dx, dz) = (b.0 - a.0, b.2 - a.2);
  (dx * dx + dz * dz).sqrt()
}

pub fn distance(a: (f32, f32, f32), b: (f32, f32, f32)) -> f32 {
  let (dx, dy, dz) = (b.0 - a.0, b.1 - a.1, b.2 - a.2);
  (dx * dx + dy * dy + dz * dz).sqrt()
}

/// Upward speed a jump starts with.
pub const JUMP_SPEED: f32 = 8.4;

/// Downward acceleration, in units per second squared.
pub const GRAVITY: f32 = 22.0;

/// How far above the ground a claim may be before the server stops believing
/// it.
///
/// A jump reaches `JUMP_SPEED^2 / (2 * GRAVITY)`, so the ceiling is that plus
/// room for a slope the client and server rounded differently. It is the one
/// check a height rule makes possible and a speed budget cannot: a client
/// flying costs no horizontal distance at all.
pub const MAX_AIR: f32 = JUMP_SPEED * JUMP_SPEED / (2.0 * GRAVITY) + 2.5;

/// A character with vertical motion, which is the client's own half of this
/// example: the server never simulates one.
#[derive(Clone, Copy, Debug)]
pub struct Body {
  pub at: (f32, f32, f32),
  pub vy: f32,
  pub grounded: bool,
}

impl Body {
  pub fn new(at: (f32, f32, f32)) -> Self {
    Self {
      at,
      vy: 0.0,
      grounded: true,
    }
  }

  /// Walks by `wish`, applies gravity, and lands on the ground.
  ///
  /// `ground` is the terrain rule, passed in rather than called directly so the
  /// tests can stand this on a flat floor and a staircase without a world.
  pub fn step(
    &mut self,
    wish: (f32, f32),
    jump: bool,
    dt: f32,
    ground: impl Fn(f32, f32) -> f32,
  ) {
    if jump && self.grounded {
      self.vy = JUMP_SPEED;
      self.grounded = false;
    }

    let (mut x, mut z) = (self.at.0 + wish.0, self.at.2 + wish.1);
    x = x.clamp(-crate::terrain::EDGE + 2.0, crate::terrain::EDGE - 2.0);
    z = z.clamp(-crate::terrain::EDGE + 2.0, crate::terrain::EDGE - 2.0);

    let floor = ground(x, z);
    // A step up a slope is a step, not a fall: walking into a hillside must
    // not launch anyone, and walking off one must not stick them to it.
    if self.grounded && (floor - self.at.1).abs() <= STEP_UP {
      self.at = (x, floor, z);
      self.vy = 0.0;
      return;
    }

    self.vy -= GRAVITY * dt;
    let y = self.at.1 + self.vy * dt;
    if y <= floor {
      self.at = (x, floor, z);
      self.vy = 0.0;
      self.grounded = true;
    } else {
      self.at = (x, y, z);
      self.grounded = false;
    }
  }
}

/// The tallest rise a walking character takes without leaving the ground.
pub const STEP_UP: f32 = 1.2;

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
  fn a_jump_leaves_the_ground_and_comes_back_to_it() {
    let flat = |_: f32, _: f32| 0.0;
    let mut body = Body::new((0.0, 0.0, 0.0));
    body.step((0.0, 0.0), true, 1.0 / 60.0, flat);
    assert!(!body.grounded, "the jump never left the ground");

    let mut peak: f32 = 0.0;
    for _ in 0..240 {
      body.step((0.0, 0.0), false, 1.0 / 60.0, flat);
      peak = peak.max(body.at.1);
      if body.grounded {
        break;
      }
    }
    assert!(body.grounded, "the jump never landed");
    assert!(peak > 1.0, "the jump only reached {peak}");
    assert!(peak < MAX_AIR, "a jump must stay inside what the server will believe");
    assert_eq!(body.at.1, 0.0);
  }

  #[test]
  fn a_second_jump_needs_the_ground_first() {
    let flat = |_: f32, _: f32| 0.0;
    let mut body = Body::new((0.0, 0.0, 0.0));
    body.step((0.0, 0.0), true, 1.0 / 60.0, flat);
    let rising = body.vy;
    body.step((0.0, 0.0), true, 1.0 / 60.0, flat);
    assert!(body.vy < rising, "a second press re-launched a body already in the air");
  }

  #[test]
  fn walking_a_slope_does_not_launch_or_stick() {
    // The case a naive ground clamp gets wrong in both directions: a rise
    // becomes a jump and a drop becomes flight.
    let hill = |x: f32, _: f32| x * 0.5;
    let mut body = Body::new((0.0, 0.0, 0.0));
    for _ in 0..120 {
      body.step((0.1, 0.0), false, 1.0 / 60.0, hill);
      assert!(body.grounded, "a walk up a slope left the ground at x={}", body.at.0);
      assert!((body.at.1 - hill(body.at.0, 0.0)).abs() < 1e-4);
    }
  }

  #[test]
  fn a_body_stays_inside_the_world() {
    let flat = |_: f32, _: f32| 0.0;
    let mut body = Body::new((0.0, 0.0, 0.0));
    for _ in 0..4000 {
      body.step((1.0, 1.0), false, 1.0 / 60.0, flat);
    }
    assert!(body.at.0 <= crate::terrain::EDGE && body.at.2 <= crate::terrain::EDGE);
  }

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
  fn a_silence_earns_distance_up_to_the_cap_and_no_further() {
    // A character really did have that long to walk, so refusing the distance
    // outright would punish a client for a gap the network caused. Banking it
    // without limit is the other failure: five minutes of silence would earn
    // the width of the zone several times over, and a disconnection would be a
    // teleport. The cap is a compromise and costs exactly what it says: a
    // client returning from a stall longer than it gets snapped back once.
    let mut within = Tracked::new((0.0, 0.0, 0.0), 0);
    assert_eq!(within.claim((20.0, 0.0, 0.0), 3_000), Verdict::Accepted);

    let mut beyond = Tracked::new((0.0, 0.0, 0.0), 0);
    assert_eq!(beyond.claim((60.0, 0.0, 0.0), 10_000), Verdict::Refused);
    assert_eq!(beyond.claim((28.0, 0.0, 0.0), 10_000), Verdict::Accepted, "up to the cap");
  }

  #[test]
  fn two_claims_between_ticks_are_not_refused_for_arriving_together() {
    // Without a credited grain this is the shape of a false positive that
    // costs the design its only signal: an honest client whose packets bunch
    // up gets refused for it, and the refusal count stops meaning anything.
    // Two halves of a tick's travel, which is what a client sending twice in a
    // tick actually reports. Two *whole* steps in no elapsed time is not a
    // bunched packet, it is twice the speed, and the budget refuses it.
    let mut t = Tracked::new((0.0, 0.0, 0.0), 0);
    let step = RUN_SPEED * (CLOCK_GRAIN_MS as f32 / 1000.0);
    assert_eq!(t.claim((step / 2.0, 0.0, 0.0), 0), Verdict::Accepted);
    assert_eq!(t.claim((step, 0.0, 0.0), 0), Verdict::Accepted, "same millisecond");
    assert_eq!(t.refusals, 0);

    let mut greedy = Tracked::new((0.0, 0.0, 0.0), 0);
    assert_eq!(greedy.claim((step, 0.0, 0.0), 0), Verdict::Accepted);
    assert_eq!(
      greedy.claim((step * 2.0, 0.0, 0.0), 0),
      Verdict::Refused,
      "two whole steps in no time is twice the speed, not a bunched packet"
    );
  }

  #[test]
  fn the_credited_grain_is_one_tick_and_not_a_free_pass() {
    // The other half, or the fix would be a hole: crediting a grain does not
    // credit a teleport.
    let mut t = Tracked::new((0.0, 0.0, 0.0), 0);
    assert_eq!(t.claim((400.0, 0.0, 0.0), 0), Verdict::Refused);
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
