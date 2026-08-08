//! A resumed session, and what it must not spend twice. Four seats delve
//! through locked rooms; a dropped link holds its seat (`ReconnectTracker`'s
//! first held-seat consumer), the party never advances past a held seat, and
//! every acting op carries a sequence the server applies at most once, so the
//! client's resend-after-resume is safe. The dedup has an off switch because
//! the panel's job is to show what a duplicate costs when nothing suppresses
//! it: a key burned on an open door.

pub mod protocol;
pub mod role;

#[cfg(feature = "server")]
pub mod logic;
#[cfg(feature = "server")]
pub mod snapshot;
#[cfg(feature = "server")]
pub mod state;

#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
pub mod net;

pub use playground_common;
