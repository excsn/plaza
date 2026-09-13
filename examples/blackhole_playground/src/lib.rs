//! A multiplayer black hole game that sends a *field* instead of its
//! consequences.
//!
//! You are a black hole. Pellets fall toward you, slowly at the rim and faster
//! the closer they get, and swallowing them makes you bigger. Running into
//! another player costs you mass.
//!
//! Thousands of pellets move only because of a handful of black holes. So the
//! server can either send the **field** (a few positions and masses) and let
//! every client integrate the pellets itself or send thousands of pellet
//! positions the conventional way. The example implements both and measures
//! the difference.
//!
//! This is deliberately a *hard* case for local simulation. The horde example's
//! enemies home toward a target, so prediction errors shrink on their own.
//! Gravity is divergent, so here they grow.

pub mod net;
pub mod role;
pub mod sim;
