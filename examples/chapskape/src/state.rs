//! What the server owns: one world, who is standing in it, and what each of
//! them has already been told about the half that does not move.
//!
//! That last part is the only bookkeeping here that is not obvious, and it is
//! the whole of the still-world argument. A viewer's memory of the props is a
//! handful of ids against the tick each comes back on. It is small because
//! almost nothing is ever out, and it is *possible* because the tick is
//! absolute: a countdown would differ every tick and there would be nothing to
//! remember.

use std::collections::HashMap;

use plaza_client_utils::FixedTimestep;
use plaza_server_utils::{Crew, Roster, Told};

use crate::bots::Bots;
use crate::controls::Relevance;
use crate::protocol::{ObjectState, PlayerId, Seat, TICK_MS};
use crate::zone::{Zone, MAX_ACTORS};

/// The most game ticks one wake-up may run.
///
/// A host that stalled and came back owing two seconds must not spend them all
/// in one frame: the world would jump and every client would watch it happen.
pub const CATCH_UP: u32 = 3;

pub struct SkapeState {
  pub zone: Zone,
  pub roster: Roster<PlayerId>,
  /// The seats the bots hold, admitted through the same roster as anybody.
  pub crew: Crew<PlayerId>,
  pub agents: HashMap<PlayerId, plaza::agent::Agent<PlayerId>>,
  pub bots: Bots,
  pub populated: bool,
  pub mode: Relevance,
  pub tick_ms: u64,
  /// The game clock's budget, drawn down in whole ticks.
  ///
  /// The host wakes far more often than the world moves, so a game tick is a
  /// budget being drawn down rather than a wake-up being answered, and the
  /// tick length stays a dial because the catch-up cap is counted in ticks.
  pub ticker: FixedTimestep,
  pub now_ms: u64,
  /// What each viewer has been told about the props, by id against the tick
  /// they come back on.
  pub told: Told<Seat, u32, u32>,
  /// Prop entries actually put on the wire.
  pub object_entries: u64,
  /// Prop entries that would have gone out if every frame carried the lot.
  ///
  /// Kept in both modes so the panel can show the comparison without anybody
  /// having to switch and remember.
  pub object_entries_repeated: u64,
  pub frames_sent: u64,
  pub private_sent: u64,
  scratch: Vec<ObjectState>,
}

impl std::fmt::Debug for SkapeState {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("SkapeState")
      .field("tick", &self.zone.tick)
      .field("actors", &self.zone.actors.len())
      .finish_non_exhaustive()
  }
}

impl Default for SkapeState {
  fn default() -> Self {
    Self::new()
  }
}

impl SkapeState {
  pub fn new() -> Self {
    Self {
      zone: Zone::new(),
      roster: Roster::new(MAX_ACTORS),
      crew: Crew::new(),
      agents: HashMap::new(),
      bots: Bots::default(),
      populated: false,
      mode: Relevance::default(),
      tick_ms: TICK_MS,
      ticker: FixedTimestep::from_step_ms(TICK_MS).with_max_steps(CATCH_UP).with_max_frame_ms(3_600_000),
      now_ms: 0,
      told: Told::new(),
      object_entries: 0,
      object_entries_repeated: 0,
      frames_sent: 0,
      private_sent: 0,
      scratch: Vec::new(),
    }
  }

  pub fn seat_of(&self, player: PlayerId) -> Option<Seat> {
    self.roster.seat_of(&player).map(|s| s as Seat)
  }

  /// What to put on this viewer's frame about the props, under the mode the
  /// world is running.
  ///
  /// In `EveryTick` this is everything out in view, which is what a visibility
  /// diff over movers would do and what the still world does not need. In
  /// `OnChange` it is the difference against what this viewer already knows,
  /// with a zero standing for "that one is back", because a client cannot infer
  /// an absence from a stream that says nothing when nothing happened.
  pub fn objects_for(&mut self, seat: Seat, middle: crate::protocol::Tile) -> Vec<ObjectState> {
    let mut visible = std::mem::take(&mut self.scratch);
    self.zone.depleted_in_view(middle, &mut visible);
    self.object_entries_repeated += visible.len() as u64;

    let out = match self.mode {
      Relevance::EveryTick => {
        self.told.forget(&seat);
        visible.clone()
      }
      Relevance::OnChange => {
        let mut changed: Vec<ObjectState> = Vec::new();
        self.told.diff(seat, visible.iter().map(|state| (state.id, state.ready_at)), |id, ready_at| {
          match ready_at {
            Some(ready_at) => changed.push(ObjectState { id, ready_at: *ready_at }),
            // What the viewer knows and no longer sees is not the same
            // question as what came back. A prop that left the view is
            // forgotten silently; one that is standing again has to be said,
            // or the client draws a stump for ever.
            None => {
              let tile = crate::world::prop_tile(id);
              if middle.steps_to(tile) <= crate::zone::VIEW {
                changed.push(ObjectState { id, ready_at: 0 });
              }
            }
          }
        });
        changed.sort_unstable_by_key(|state| state.id);
        changed
      }
    };

    self.object_entries += out.len() as u64;
    visible.clear();
    self.scratch = visible;
    out
  }

  pub fn forget(&mut self, seat: Seat) {
    self.told.forget(&seat);
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::protocol::Tile;
  use crate::world;

  fn a_prop_near(middle: Tile) -> u32 {
    for radius in 1..24i16 {
      for dy in -radius..=radius {
        for dx in -radius..=radius {
          let tile = Tile::new(middle.x + dx, middle.y + dy);
          if world::prop_at(tile).is_some() {
            return world::prop_id(tile);
          }
        }
      }
    }
    panic!("no props near the middle of the world");
  }

  #[test]
  fn a_change_only_stream_says_a_depletion_once() {
    // The whole of the still-world argument in one assertion: the second frame
    // costs nothing, and so does the hundredth.
    let mut state = SkapeState::new();
    state.mode = Relevance::OnChange;
    let middle = world::the_green();
    let id = a_prop_near(middle);

    assert!(state.objects_for(1, middle).is_empty(), "an untouched world said something");
    state.zone.depleted.insert(id, 40);
    let first = state.objects_for(1, middle);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0], ObjectState { id, ready_at: 40 });

    for _ in 0..10 {
      assert!(state.objects_for(1, middle).is_empty(), "it was said twice");
    }
  }

  #[test]
  fn a_prop_coming_back_has_to_be_said_out_loud() {
    // The failure a change-only stream invites: silence means nothing happened,
    // so a client that was told about a stump and never told otherwise draws
    // one for the rest of the session.
    let mut state = SkapeState::new();
    state.mode = Relevance::OnChange;
    let middle = world::the_green();
    let id = a_prop_near(middle);

    state.zone.depleted.insert(id, 40);
    state.objects_for(1, middle);
    state.zone.depleted.remove(&id);
    let back = state.objects_for(1, middle);
    assert_eq!(back, vec![ObjectState { id, ready_at: 0 }], "the client was never told");
    assert!(state.objects_for(1, middle).is_empty(), "and then it was told again");
  }

  #[test]
  fn every_tick_repeats_itself_and_change_only_does_not() {
    // Both counters are kept in both modes, so the comparison does not depend
    // on somebody switching and remembering what the other one said.
    let middle = world::the_green();
    let id = a_prop_near(middle);

    let mut repeated = SkapeState::new();
    repeated.mode = Relevance::EveryTick;
    repeated.zone.depleted.insert(id, 400);
    let mut quiet = SkapeState::new();
    quiet.mode = Relevance::OnChange;
    quiet.zone.depleted.insert(id, 400);

    for _ in 0..50 {
      repeated.objects_for(1, middle);
      quiet.objects_for(1, middle);
    }
    assert_eq!(repeated.object_entries, 50);
    assert_eq!(quiet.object_entries, 1);
    assert_eq!(
      repeated.object_entries_repeated, quiet.object_entries_repeated,
      "the two modes disagree about what there was to send"
    );
  }

  #[test]
  fn two_viewers_are_told_separately() {
    // A viewer's memory is a viewer's, and sharing one would mean the second
    // client to arrive is never told about anything the first already knows.
    let mut state = SkapeState::new();
    state.mode = Relevance::OnChange;
    let middle = world::the_green();
    let id = a_prop_near(middle);
    state.zone.depleted.insert(id, 40);

    assert_eq!(state.objects_for(1, middle).len(), 1);
    assert_eq!(state.objects_for(2, middle).len(), 1, "the second viewer heard nothing");
    assert!(state.objects_for(1, middle).is_empty());
  }
}
