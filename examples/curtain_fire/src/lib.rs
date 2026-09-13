//! A bullet-hell shmup where a wrong answer is a death, which cannot be eased.
//!
//! Every other prediction example here corrects a *position*: you drew a
//! player a few pixels off or on the wrong cell and the fix is to move them. A
//! bullet-hell ship is killed by a single pixel of contact, so a wrong answer
//! costs a life. A death cannot be smoothed or rewound afterwards.
//!
//! So this example is about **who is allowed to say you were hit**. It has
//! three rules and a switch between them and the panel prints the number that
//! shows each one's weakness.
//!
//! It also carries a second measurement. The enemy curtain is a closed-form
//! function of the tick, so each wave costs one announcement however many
//! thousand bullets it produces; player fire depends on a human and keeps
//! costing bytes. The panel shows the price of each half.
//!
//! Start with [`sim::curtain`], then [`sim::server::Server::judge_deaths`].

pub mod sim;

#[cfg(any(feature = "server", all(feature = "client", feature = "websocket")))]
pub mod net;

pub mod role;

pub use playground_common;
