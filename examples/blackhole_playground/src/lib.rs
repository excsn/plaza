//! A multiplayer black hole game, and a study in sending a *field* instead of
//! its consequences.
//!
//! You are a black hole. Pellets fall toward you, slowly at the rim and faster
//! the closer they get, and swallowing them makes you bigger. Running into
//! another player costs you mass, so the map is a chase with a penalty for
//! contact.
//!
//! The netcode question underneath: thousands of pellets move entirely because
//! of a handful of black holes. So the server can send the **field** (a few
//! positions and masses) and let every client integrate the pellets itself, or
//! it can send thousands of pellet positions the conventional way. The example
//! implements both and measures the difference.
//!
//! It is deliberately the *hard* case for local simulation. The horde example's
//! enemies home toward a target, so prediction errors shrink on their own;
//! gravity is divergent, so they grow. That is what makes it a useful second
//! consumer.

pub mod net;
pub mod role;
pub mod sim;
