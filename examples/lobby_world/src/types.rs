use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type PlayerId = u64;
pub type RoomId = Uuid;

/// Server-measured throughout. A client-reported latency would be understated,
/// and this decides admission.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct LinkQuality {
  pub measured_rtt_ms: u32,
  /// Assigned on connect, so a localhost demo still has slow links in it.
  pub assigned_extra_ms: u32,
  /// What admission is judged against.
  pub one_way_ms: u32,
}

impl LinkQuality {
  pub fn new(measured_rtt_ms: u32, assigned_extra_ms: u32) -> Self {
    Self {
      measured_rtt_ms,
      assigned_extra_ms,
      one_way_ms: measured_rtt_ms / 2 + assigned_extra_ms,
    }
  }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ArenaSettings {
  /// Tenths of a second between pot refreshes.
  pub refresh_decis: u32,
  pub budget_ms: Option<u32>,
}

pub struct Arena {
  pub name: &'static str,
  pub max_players: u32,
  pub settings: ArenaSettings,
}

pub const ARENAS: [Arena; 3] = [
  Arena {
    name: "sprint",
    max_players: 2,
    settings: ArenaSettings {
      refresh_decis: 8,
      budget_ms: Some(30),
    },
  },
  Arena {
    name: "cruise",
    max_players: 3,
    settings: ArenaSettings {
      refresh_decis: 15,
      budget_ms: Some(90),
    },
  },
  Arena {
    name: "drift",
    max_players: 4,
    settings: ArenaSettings {
      refresh_decis: 25,
      budget_ms: None,
    },
  },
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomCard {
  pub room_id: RoomId,
  pub name: String,
  pub current_players: u32,
  pub max_players: u32,
  pub budget_ms: Option<u32>,
  pub playable: bool,
  /// Position in `rooms_playable_at`, or `None` if this link cannot carry it.
  pub fit_rank: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LobbyOp {
  ListRooms,
  Join { room_id: RoomId },
  /// Deliberately bypasses `handle_join_room_request`: a spectator takes no
  /// seat, so the lobby's capacity accounting must not see one.
  Spectate { room_id: RoomId },
  Reroll,

  Welcome {
    you: PlayerId,
    link: LinkQuality,
    coins: u64,
  },
  Catalogue {
    rooms: Vec<RoomCard>,
    link: LinkQuality,
  },
  Placed {
    room_id: RoomId,
    name: String,
    endpoint: String,
    spectator: bool,
    coins: u64,
  },
  Refused {
    room_id: RoomId,
    reason: String,
    measured_one_way_ms: u32,
    allowed_one_way_ms: Option<u32>,
  },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Seat {
  Player,
  Spectator,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Occupant {
  pub player: PlayerId,
  pub seat: Seat,
  /// From the shared registry, so it includes what was earned elsewhere.
  pub coins: u64,
  pub claims_here: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomView {
  pub arena: String,
  pub budget_ms: Option<u32>,
  pub pot: u64,
  pub occupants: Vec<Occupant>,
  pub seats_taken: u32,
  pub seats_total: u32,
  pub spectators: u32,
  pub your_seat: Option<Seat>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RoomOp {
  Claim,

  /// System-only; the arena rejects it from a client, since nothing else stands
  /// between a client and a free seat.
  Reserve { player: PlayerId },

  /// System-only. Cancelling on a closing socket instead would lose the seat:
  /// a room hop closes the old connection after the new seat is reserved.
  Withdraw { player: PlayerId },

  /// Boxed, or every `RoomOp` in a batch is sized to a whole view.
  Snapshot(Box<RoomView>),
  Claimed {
    player: PlayerId,
    amount: u64,
    coins: u64,
  },
  PotRefreshed {
    pot: u64,
  },
  Rejected {
    reason: String,
  },
}
