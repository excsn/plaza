//! A zone of characters, and what one client is told about them.
//!
//! The point of assembling it is that the four things this example measures
//! separately have to hold together: a character the client owns and the server
//! only checks, an audience built from two channels rather than one, and a cast
//! that is a wait the design already asked for.

use std::collections::HashMap;

use crate::casting::{Ms, GLOBAL_COOLDOWN_MS};
use crate::movement::{Tracked, Verdict};
use crate::relevance::{audience, Audience, Parties, Seat};

/// How far a character is told about, in metres.
pub const VIEW: f32 = 30.0;
/// Metres between floors, which is what makes a zone a building rather than a
/// field.
pub const FLOOR_HEIGHT: f32 = 5.0;

/// What a character is doing, if anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cast {
  pub ability: u8,
  /// When it will land, on the server's clock.
  pub lands_at: Ms,
}

#[derive(Clone, Copy, Debug)]
pub struct Character {
  pub seat: Seat,
  /// Where the **client** says it is, which the server only sanity-checks.
  pub tracked: Tracked,
  pub health: u16,
  pub casting: Option<Cast>,
  /// When this seat may act again, which is the other half of the design
  /// absorbing latency.
  pub ready_at: Ms,
  pub alive: bool,
}

impl Character {
  pub fn new(seat: Seat, at: (f32, f32, f32), now_ms: Ms) -> Self {
    Self {
      seat,
      tracked: Tracked::new(at, now_ms),
      health: 100,
      casting: None,
      ready_at: 0,
      alive: true,
    }
  }
}

#[derive(Default)]
pub struct Zone {
  pub characters: HashMap<Seat, Character>,
  pub parties: Parties,
  pub now_ms: Ms,
  /// Claims the validator refused, which is the only signal there is that
  /// somebody is not playing the same game.
  pub refusals: u64,
  /// Casts that landed, for the panel.
  pub landed: u64,
}

impl Zone {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn admit(&mut self, seat: Seat, at: (f32, f32, f32)) {
    self.characters.insert(seat, Character::new(seat, at, self.now_ms));
  }

  pub fn remove(&mut self, seat: Seat) {
    self.characters.remove(&seat);
    // Leaving the zone leaves the party, or a health bar keeps updating for
    // somebody who is not here.
    self.parties.leave(seat);
  }

  /// Takes a claimed position, or does not.
  pub fn claim(&mut self, seat: Seat, to: (f32, f32, f32)) -> Verdict {
    let now = self.now_ms;
    let Some(character) = self.characters.get_mut(&seat) else {
      return Verdict::Refused;
    };
    let verdict = character.tracked.claim(to, now);
    if verdict == Verdict::Refused {
      self.refusals += 1;
    }
    verdict
  }

  /// Begins a cast, if this seat is allowed to act.
  ///
  /// Refused while already casting or inside the cooldown, which is not a rate
  /// limit bolted on: it is the wait the whole latency argument rests on.
  pub fn begin_cast(&mut self, seat: Seat, ability: u8, cast_ms: Ms) -> bool {
    let now = self.now_ms;
    let Some(character) = self.characters.get_mut(&seat) else {
      return false;
    };
    if character.casting.is_some() || now < character.ready_at {
      return false;
    }
    character.casting = Some(Cast {
      ability,
      lands_at: now + cast_ms,
    });
    character.ready_at = now + cast_ms.max(GLOBAL_COOLDOWN_MS);
    true
  }

  /// Advances the clock and lands whatever was due.
  ///
  /// Returns the seats whose cast landed, because that is an **event**: it
  /// happens once and no later frame mentions it, which is the same shape
  /// poketo's kills have and the same reason it has to be delivered rather
  /// than described.
  pub fn advance(&mut self, dt_ms: Ms) -> Vec<Seat> {
    self.now_ms += dt_ms;
    let now = self.now_ms;
    let mut landed = Vec::new();
    for character in self.characters.values_mut() {
      let Some(cast) = character.casting else {
        continue;
      };
      if now >= cast.lands_at {
        character.casting = None;
        landed.push(character.seat);
      }
    }
    self.landed += landed.len() as u64;
    landed
  }

  /// Who is within the view radius of a seat, itself included.
  pub fn near(&self, seat: Seat, out: &mut Vec<Seat>) {
    out.clear();
    let Some(from) = self.characters.get(&seat).filter(|c| c.alive) else {
      return;
    };
    for character in self.characters.values().filter(|c| c.alive) {
      if crate::movement::distance(from.tracked.at, character.tracked.at) <= VIEW {
        out.push(character.seat);
      }
    }
    out.sort_unstable();
  }

  /// Everything a seat is told about this tick: near, plus subscribed.
  pub fn audience_for(&self, seat: Seat, scratch: &mut Vec<Seat>) -> Audience {
    self.near(seat, scratch);
    audience(scratch, &self.parties, seat)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn zone() -> Zone {
    let mut zone = Zone::new();
    zone.admit(1, (0.0, 0.0, 0.0));
    zone.admit(2, (5.0, 0.0, 0.0));
    // Three floors up and across the zone: near nobody.
    zone.admit(3, (200.0, FLOOR_HEIGHT * 3.0, 200.0));
    zone
  }

  #[test]
  fn an_audience_is_the_two_channels_unioned() {
    let mut zone = zone();
    let mut scratch = Vec::new();

    let a = zone.audience_for(1, &mut scratch);
    assert_eq!(a.seats, vec![1, 2], "distance alone");

    zone.parties.join(1, 3);
    let a = zone.audience_for(1, &mut scratch);
    assert_eq!(a.seats, vec![1, 2, 3], "and a party member across the zone");
    assert_eq!(a.subscribed, 1);
  }

  #[test]
  fn leaving_the_zone_leaves_the_party() {
    // Or a health bar keeps updating for somebody who is not here, which is a
    // subscription outliving the thing it is about.
    let mut zone = zone();
    zone.parties.join(1, 3);
    zone.remove(3);
    let mut scratch = Vec::new();
    let a = zone.audience_for(1, &mut scratch);
    assert_eq!(a.subscribed, 0);
    assert_eq!(a.seats, vec![1, 2]);
  }

  #[test]
  fn a_cast_cannot_be_started_twice() {
    let mut zone = zone();
    assert!(zone.begin_cast(1, 0, 1500), "the first one goes");
    assert!(!zone.begin_cast(1, 0, 1500), "not while already casting");

    let landed = zone.advance(1500);
    assert_eq!(landed, vec![1], "and it lands once");
  }

  #[test]
  fn the_cooldown_runs_during_a_cast_rather_than_after_it() {
    // Which is why a caster is not waiting three seconds between long
    // abilities: the two waits overlap, and the longer of them is the whole of
    // it. A short cast is the case where the cooldown is still running when
    // the ability has already gone off.
    let mut long = zone();
    long.begin_cast(1, 0, GLOBAL_COOLDOWN_MS);
    long.advance(GLOBAL_COOLDOWN_MS);
    assert!(long.begin_cast(1, 0, 0), "a full-length cast is ready the moment it lands");

    let mut short = zone();
    short.begin_cast(1, 0, 400);
    short.advance(400);
    assert!(!short.begin_cast(1, 0, 400), "a short one still owes the cooldown");
    short.advance(GLOBAL_COOLDOWN_MS - 400);
    assert!(short.begin_cast(1, 0, 400));
  }

  #[test]
  fn an_instant_ability_still_waits_for_the_cooldown() {
    // Which is why an instant is not an exception to the design absorbing
    // latency: the next input was never going to be frame-tight.
    let mut zone = zone();
    assert!(zone.begin_cast(1, 1, 0));
    zone.advance(1);
    assert!(!zone.begin_cast(1, 1, 0), "the cooldown is the wait now");
    zone.advance(GLOBAL_COOLDOWN_MS);
    assert!(zone.begin_cast(1, 1, 0));
  }

  #[test]
  fn a_landing_is_reported_once_and_never_again() {
    // An event rather than a state, the same shape poketo's kills have: a
    // client that misses it is not told again by anything.
    let mut zone = zone();
    zone.begin_cast(1, 0, 500);
    assert!(zone.advance(400).is_empty(), "not yet");
    assert_eq!(zone.advance(200), vec![1], "now");
    assert!(zone.advance(1000).is_empty(), "and not a second time");
  }

  #[test]
  fn a_refused_claim_is_counted_because_it_is_the_only_signal() {
    let mut zone = zone();
    assert_eq!(zone.claim(1, (1.0, 0.0, 0.0)), Verdict::Refused, "no time has passed");
    assert_eq!(zone.refusals, 1);

    zone.advance(1000);
    assert_eq!(zone.claim(1, (1.0, 0.0, 0.0)), Verdict::Accepted, "a second is plenty");
    assert_eq!(zone.refusals, 1);
  }
}
