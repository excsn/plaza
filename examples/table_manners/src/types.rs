use serde::{Deserialize, Serialize};

pub type Seat = u32;

/// Seats at the party.
pub const SEATS: usize = 4;
/// Silence after which the table assumes you have wandered off.
pub const AFK_SECS: u64 = 3;
/// Inbound ops per window before the table stops being polite about it.
pub const FLOOD_OPS: u64 = 40;
pub const FLOOD_WINDOW_MS: u64 = 1000;

/// Why a seat was vacated. The distinction the whole example turns on: a drop
/// keeps your seat warm, and a kick does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Parting {
  /// The network went away. Come back and the seat is still yours.
  Dropped,
  /// The host removed you. The seat is gone and so is the grace.
  Kicked,
  /// You stopped talking to us.
  Afk,
  /// You would not stop talking to us.
  Flooding,
  /// The party is over for everybody.
  Drained,
}

impl Parting {
  pub fn as_str(self) -> &'static str {
    match self {
      Parting::Dropped => "connection lost",
      Parting::Kicked => "removed by the host",
      Parting::Afk => "away from the table",
      Parting::Flooding => "flooding",
      Parting::Drained => "the party has ended",
    }
  }

  /// Whether a seat survives this, which is what `ReconnectTracker` is for.
  pub fn keeps_the_seat(self) -> bool {
    matches!(self, Parting::Dropped)
  }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PartyOp {
  // Client to server.
  /// Which seat this connection claims.
  Sit { seat: Seat },
  /// Anything a guest does. Also what resets the AFK clock.
  Say(String),
  /// Host tools.
  Kick { seat: Seat },
  EndParty,

  // Server to client.
  /// You are leaving, and this is why. Written before the socket shuts.
  Farewell { reason: Parting, detail: String },
  Seated { seat: Seat },
  /// What everyone can see, including the moderation panel.
  Snapshot(Box<Table>),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Guest {
  pub seat: Seat,
  pub said: u32,
  pub quiet_for_ms: u64,
  pub ops_this_window: u64,
  pub griefer: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Table {
  pub guests: Vec<Guest>,
  /// Seats held open for someone who dropped, with the grace left.
  pub held: Vec<(Seat, u64)>,
  pub ended: bool,
}
