//! Which rooms a connection can actually play in, and in what order to offer
//! them.
//!
//! A free function over metadata rather than a method on the manager, because
//! the rule is worth having without one. A game whose rooms are a fixed table
//! decided at startup should not have to implement a `RoomFactory` to ask this
//! question, and the manager's own
//! [`rooms_playable_at`](crate::manager::InMemoryLobbyManager::rooms_playable_at)
//! is this with its rooms filled in.

use std::fmt::Debug;

use crate::op_payloads::RoomMetadata;

/// The rooms this connection could play in, best fit first.
///
/// **Ordered tightest-schedule-first**, so a fast connection is not sent to the
/// room built for slow ones and made to pay a delay it does not need. A room
/// that states no limit sorts last: it will take anybody, which makes it the
/// fallback rather than the first choice.
///
/// Full rooms are dropped, because offering a room somebody cannot enter is the
/// same unhelpfulness as offering one they cannot play in.
pub fn playable_at<S: Clone + Debug>(one_way_ms: u32, rooms: impl IntoIterator<Item = RoomMetadata<S>>) -> Vec<RoomMetadata<S>> {
  let mut rooms: Vec<_> = rooms
    .into_iter()
    .filter(|m| m.current_players < m.max_players)
    .filter(|m| m.max_one_way_ms.is_none_or(|allowed| one_way_ms <= allowed))
    .collect();
  rooms.sort_by_key(|m| m.max_one_way_ms.unwrap_or(u32::MAX));
  rooms
}

/// The single best room for this connection, or `None` when nothing fits.
///
/// `None` is the only case that justifies refusing somebody. Everything else is
/// a placement, which is the whole reason this decision belongs to a lobby: a
/// room can only say yes or no, and this can say *where*.
pub fn best_for<S: Clone + Debug>(one_way_ms: u32, rooms: impl IntoIterator<Item = RoomMetadata<S>>) -> Option<RoomMetadata<S>> {
  playable_at(one_way_ms, rooms).into_iter().next()
}
