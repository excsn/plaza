//! Plaza Lobby: components for building single-server lobby and room
//! management on top of Plaza Core.
//!
//! Implement [`RoomFactory`] for your game, hand it to an
//! [`InMemoryLobbyManager`], and the manager handles room creation, listing,
//! join authorization, and reaping finished rooms.
//!
//! Four smaller pieces cover what sits either side of that, each holding no
//! timers and spawning nothing, so an application drives them from its own
//! `StateLogic`:
//!
//! - [`MatchQueue`] for games where a player is paired rather than choosing,
//!   including filling the seats nobody came for.
//! - [`SeatReservations`] for the window between a lobby admitting a player and
//!   that player's socket arriving.
//! - [`TicketRegistry`] so a room learns who connected from the lobby rather
//!   than from the client.
//! - [`routing`] for placing a connection in the room whose schedule fits it.

pub mod error;
pub mod factory;
pub mod manager;
pub mod op_payloads;
pub mod queue;
pub mod reservations;
pub mod room;
pub mod routing;
pub mod tickets;
pub mod types;

pub use error::LobbyError;
pub use factory::RoomFactory;
pub use manager::{InMemoryLobbyManager, PasswordVerifier};
pub use op_payloads::*;
pub use queue::{Formed, MatchQueue};
pub use reservations::SeatReservations;
pub use room::{InProcessRoomHandle, RoomHandle};
pub use tickets::{Ticket, TicketRegistry};
pub use types::{GameMode, RoomId};
