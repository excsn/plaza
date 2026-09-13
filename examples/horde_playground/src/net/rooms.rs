//! More than one arena, differing in the thing a connection has to fit.
//!
//! An arena that schedules inputs ahead can only carry a connection whose delay
//! fits inside the schedule. One arena means one budget, so everybody past it is
//! turned away, which is how admission started. Several arenas at several depths
//! turn it into a placement: a slow link gets the one built for it and only a
//! link past *every* budget is actually refused.
//!
//! The rooms differ in exactly one setting. A deeper playout delay carries a
//! worse connection and costs everybody in that room more input lag, so the
//! table below makes that one trade at three depths rather than being a set of
//! unrelated presets.

use crate::sim::types::{Controls, SIM_STEP_MS};

/// One arena's identity and what it can carry.
#[derive(Clone, Copy, Debug)]
pub struct Room {
  pub id: u32,
  pub name: &'static str,
  /// How long this arena holds an input before executing it. This is the only
  /// setting the rooms differ in and the reason they carry different links.
  pub playout_delay_ms: u64,
}

impl Room {
  /// The worst one-way delay this room can take.
  ///
  /// Derived from the schedule rather than declared beside it: an input is named
  /// for `press + playout_delay` and rejected once it lands more than
  /// `input_max_late_ticks` past it, so this *is* the condition and it moves
  /// with the settings instead of drifting out of step with them.
  pub fn budget_ms(&self, controls: &Controls) -> u32 {
    (self.playout_delay_ms + controls.input_max_late_ticks * SIM_STEP_MS) as u32
  }

  /// Where a client plays this room.
  pub fn endpoint(&self) -> String {
    format!("/ws/{}", self.id)
  }

  /// This room's settings, which are the arena's defaults with its own depth.
  pub fn controls(&self, base: Controls) -> Controls {
    Controls {
      playout_delay_ms: self.playout_delay_ms,
      ..base
    }
  }
}

/// Every arena this example knows how to run, **in the order they are worth
/// adding**.
///
/// Not sorted by depth. The first is the arena that used to be the only one, so
/// a default run is exactly what it was. The second to add is the *relaxed* one,
/// because it accepts links that would otherwise be refused outright. A sharper
/// room only helps players who already had somewhere to play.
const ALL: [Room; 3] = [
  Room {
    id: 0,
    name: "standard",
    playout_delay_ms: 100,
  },
  Room {
    id: 1,
    name: "relaxed",
    playout_delay_ms: 300,
  },
  Room {
    id: 2,
    name: "sharp",
    playout_delay_ms: 50,
  },
];

/// The arenas to actually run.
///
/// **One by default.** Each room is a whole simulation of thousands of enemies
/// at 60 Hz, so a local run would pay three times over for a spread of latency
/// that a single player on one machine does not have. Extra rooms are worth
/// their cost once real connections arrive, which is why this is an argument
/// rather than a constant: see `--rooms`.
pub fn active(count: usize) -> &'static [Room] {
  &ALL[..count.clamp(1, ALL.len())]
}

/// The room a host plays in, and the one whose settings the panel edits.
pub const DEFAULT_ROOM: usize = 0;

pub fn room(id: u32) -> Option<&'static Room> {
  ALL.iter().find(|r| r.id == id)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn one_room_by_default_and_it_is_the_arena_this_example_always_had() {
    // The default has to stay cheap. Each room is a whole simulation of
    // thousands of enemies at 60 Hz, so running the full set locally triples the
    // server cost for a spread of latency a single player does not have.
    let active = active(1);
    assert_eq!(active.len(), 1, "a local run pays for one arena");
    assert_eq!(active[0].name, "standard");
    assert_eq!(active[0].playout_delay_ms, 100, "and it is exactly the arena that used to be the only one");
  }

  #[test]
  fn the_second_room_worth_running_is_the_one_that_rescues_refusals() {
    // A second arena exists to take links that would otherwise be turned away,
    // so it is the *deeper* one. A sharper room only helps players who already
    // had somewhere to play.
    let two = active(2);
    assert_eq!(two[1].name, "relaxed");
    assert!(two[1].playout_delay_ms > two[0].playout_delay_ms, "it carries worse connections, which is the whole point of adding it");
  }

  #[test]
  fn a_budget_is_derived_from_the_schedule_that_enforces_it() {
    // A constant beside the schedule would drift out of step with it and
    // advertise a capacity the arena does not have.
    let controls = Controls::default();
    let deeper = ALL.iter().find(|r| r.name == "relaxed").unwrap();
    let tighter = ALL.iter().find(|r| r.name == "sharp").unwrap();
    assert!(deeper.budget_ms(&controls) > tighter.budget_ms(&controls));
    assert_eq!(
      deeper.budget_ms(&controls) - tighter.budget_ms(&controls),
      (deeper.playout_delay_ms - tighter.playout_delay_ms) as u32,
      "the difference between two rooms' budgets is exactly the difference in their schedules"
    );
  }

  #[test]
  fn asking_for_more_rooms_than_exist_is_not_an_error() {
    assert_eq!(active(99).len(), ALL.len());
    assert_eq!(active(0).len(), 1, "and zero still runs one, since a host with no arena serves nobody");
  }
}
