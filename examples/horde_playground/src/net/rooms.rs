//! More than one arena, differing in the thing a connection has to fit.
//!
//! An arena that schedules inputs ahead can only carry a connection whose delay
//! fits inside the schedule, and one arena means one budget, so everybody past
//! it is turned away. That is the door slam admission started as. Several
//! arenas, at several depths, turn it into a placement: a slow link gets the one
//! built for it and only a link past *every* budget is actually refused.
//!
//! The rooms differ in exactly one setting on purpose. A deeper playout delay
//! carries a worse connection and costs everybody in that room more input lag,
//! so the table below is the trade written out three times rather than a set of
//! unrelated presets.

use crate::sim::types::Controls;

/// The step, in ms, for turning the late window into a time budget.
const SIM_STEP_MS: u64 = (crate::sim::types::SIM_DT * 1000.0) as u64;

/// One arena's identity and what it can carry.
#[derive(Clone, Copy, Debug)]
pub struct Room {
  pub id: u32,
  pub name: &'static str,
  /// How long this arena holds an input before executing it. The whole
  /// difference between the rooms, and the reason they carry different links.
  pub playout_delay_ms: u64,
}

impl Room {
  /// The worst one-way delay this room can take.
  ///
  /// Derived from the schedule rather than declared beside it: an input is named
  /// for `press + playout_delay` and rejected once it lands more than
  /// `input_max_late_ticks` past it, so this *is* the condition, and it moves
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

/// The arenas this host runs.
///
/// Three is enough to make placement mean something and few enough that the
/// whole set is legible. The middle one is the arena that used to be the only
/// one, so the default experience is unchanged and the others are the tails.
pub const ROOMS: [Room; 3] = [
  Room {
    id: 0,
    name: "sharp",
    playout_delay_ms: 50,
  },
  Room {
    id: 1,
    name: "standard",
    playout_delay_ms: 100,
  },
  Room {
    id: 2,
    name: "relaxed",
    playout_delay_ms: 300,
  },
];

/// The room a host or an offline build starts in, and the one whose settings the
/// panel edits.
pub const DEFAULT_ROOM: usize = 1;

pub fn room(id: u32) -> Option<&'static Room> {
  ROOMS.iter().find(|r| r.id == id)
}
