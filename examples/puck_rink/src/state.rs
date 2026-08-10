//! The authoritative state. Only [`crate::logic::RinkLogic`] mutates it.

use std::collections::HashMap;

use plaza::agent::Agent;
use plaza_server_utils::{InputSchedule, InputWindow, Roster, SeatState};

use crate::physics::Body;
use crate::protocol::{Occupant, Physics, PlayerId};
use crate::sim::{PaddleInput, World, SEATS};

/// How far behind an input may name its tick and still buffer, and how far
/// ahead. Tight on the late side: a rink rewards presence, not history.
pub const WINDOW: InputWindow = InputWindow {
  max_late: 6,
  max_early: 30,
};

#[derive(Debug)]
pub struct RinkState {
  pub body: Body,
  pub tick: u64,
  /// Every seat always has an actor: an open seat is bot-driven, so the rink
  /// is never short a paddle.
  pub roster: Roster<PlayerId>,
  pub schedules: [InputSchedule<PaddleInput>; SEATS],
  /// The level each human seat currently holds; a frame with nothing due
  /// repeats it.
  pub held: [PaddleInput; SEATS],
  pub agents: HashMap<PlayerId, Agent<PlayerId>>,
}

impl Default for RinkState {
  fn default() -> Self {
    Self::new()
  }
}

impl RinkState {
  pub fn new() -> Self {
    Self::on(Physics::Fx)
  }

  /// # Panics
  /// Panics if this build cannot run `physics`, which is a startup argument
  /// rather than anything a client can ask for.
  pub fn on(physics: Physics) -> Self {
    Self {
      body: Body::new(physics).expect("this build was not compiled with that physics backend"),
      tick: 0,
      roster: Roster::new(SEATS),
      schedules: std::array::from_fn(|_| InputSchedule::new()),
      held: [PaddleInput::default(); SEATS],
      agents: HashMap::new(),
    }
  }

  /// The wire's and the bots' view of the ice.
  pub fn world(&self) -> World {
    self.body.view()
  }

  pub fn seat_of(&self, player: PlayerId) -> Option<usize> {
    self.roster.seat_of(&player)
  }

  /// The wire's view of the seats: whoever is not a human is the bot.
  pub fn occupants(&self) -> [Occupant; SEATS] {
    std::array::from_fn(|seat| match self.roster.seat_state(seat) {
      SeatState::Human(id) => Occupant::Human(*id),
      _ => Occupant::Bot,
    })
  }
}
