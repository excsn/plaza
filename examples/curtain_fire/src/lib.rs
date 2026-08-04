//! Death is the one correction you cannot ease.
//!
//! Every other prediction example here corrects a *position*: you drew a
//! player a few pixels off, or on the wrong cell, and the fix is to move them.
//! A bullet-hell ship is killed by a single pixel of contact, so the wrong
//! answer is not a position but a life. There is no smoothing that, no
//! rewinding it, and no apologising for it afterwards.
//!
//! So this example asks the question none of the others do: **who is allowed
//! to say you were hit?** It has three answers and a switch between them, and
//! the panel prints the number that condemns each one.
//!
//! It carries a second measurement because the shape suits it. The enemy
//! curtain is a closed-form function of the tick, so it costs a wave
//! announcement and nothing else however many thousand bullets it becomes;
//! player fire depends on a human and costs bytes for ever. Both halves are on
//! one screen with a price beside each.
//!
//! Read [`sim::curtain`] first, then [`sim::server::Server::judge_deaths`].

pub mod sim;

#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
pub mod net;

pub mod role;

pub use playground_common;
