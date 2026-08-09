//! The authoritative state. Only [`crate::logic::RunLogic`] mutates it.

use std::collections::HashMap;

use plaza::agent::Agent;
use plaza::common::reconnect::ReconnectTracker;
use plaza_server_utils::{Roster, SeatState};

use crate::protocol::{Meters, PlayerId, Presence, RunView, SeatView, CHEST_KEYS, DEFAULT_GRACE_MS, ROOMS, SEATS, TICK_MS};

#[derive(Debug)]
pub struct Seat {
  pub player: PlayerId,
  pub keys: u8,
  pub coins: u32,
  pub pocketed: bool,
  /// The newest applied sequence for this seat. The dedup line: an arriving
  /// sequence at or below it has already happened.
  pub acked_seq: u64,
}

#[derive(Debug)]
pub struct RunState {
  pub room: u8,
  pub door_locked: bool,
  pub chest_keys: u8,
  pub seats: Vec<Seat>,
  /// The seat lifecycle: who is seated, whose seat is held. The tracker below
  /// is the clock that decides when a hold ends; this is what a hold *is*.
  pub roster: Roster<PlayerId>,
  pub agents: HashMap<PlayerId, Agent<PlayerId>>,

  /// The grace machinery, plaza's own: held seats, driven from the tick.
  pub tracker: ReconnectTracker<PlayerId, u64>,
  pub grace_ticks: u64,
  /// A dial change lands when no hold is running, so a window in flight keeps
  /// the terms it started under.
  pub pending_grace_ticks: Option<u64>,

  pub dedup_on: bool,
  pub meters: Meters,

  pub runs_completed: u32,
  pub complete: bool,
  pub intermission_left: Option<u64>,
  /// Ticks a lone human has been waiting for company.
  pub bot_wait: u64,
  /// Hirelings can be kept out entirely, for tests that count seats.
  pub bots_enabled: bool,

  pub tick: u64,
}

impl Default for RunState {
  fn default() -> Self {
    Self::new()
  }
}

impl RunState {
  pub fn new() -> Self {
    let grace_ticks = DEFAULT_GRACE_MS / TICK_MS;
    Self {
      room: 1,
      door_locked: true,
      chest_keys: CHEST_KEYS,
      seats: Vec::new(),
      roster: Roster::new(SEATS).holding_seats(),
      agents: HashMap::new(),
      tracker: ReconnectTracker::new(grace_ticks),
      grace_ticks,
      pending_grace_ticks: None,
      dedup_on: true,
      meters: Meters::default(),
      runs_completed: 0,
      complete: false,
      intermission_left: None,
      bot_wait: 0,
      bots_enabled: true,
      tick: 0,
    }
  }

  pub fn seat_of(&self, player: PlayerId) -> Option<usize> {
    self.seats.iter().position(|s| s.player == player)
  }

  pub fn any_seat_held(&self) -> bool {
    self.roster.seats().any(|state| matches!(state, SeatState::Held(_)))
  }

  pub fn view(&self) -> RunView {
    RunView {
      room: self.room,
      rooms: ROOMS,
      door_locked: self.door_locked,
      chest_keys: self.chest_keys,
      seats: self
        .seats
        .iter()
        .map(|s| SeatView {
          player: s.player,
          presence: match self.tracker.deadline_for(&s.player) {
            Some(deadline) => Presence::Grace {
              ms_left: deadline.saturating_sub(self.tick) * TICK_MS,
            },
            None => Presence::Here,
          },
          keys: s.keys,
          coins: s.coins,
          pocketed: s.pocketed,
          acked_seq: s.acked_seq,
        })
        .collect(),
      dedup_on: self.dedup_on,
      grace_ms: self.grace_ticks * TICK_MS,
      meters: self.meters,
      runs_completed: self.runs_completed,
      complete: self.complete,
      intermission_ms_left: self.intermission_left.map(|t| t * TICK_MS),
    }
  }
}
