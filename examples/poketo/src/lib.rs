//! A tile world, and two netcode regimes in one game.
//!
//! The overworld is real-time and discrete: a trainer occupies one tile or the
//! next, so a position is an index rather than a measurement and there is
//! nothing to quantise. Battles are turn-based and instanced, which is the
//! opposite of everything else in this tree: no prediction, no interpolation,
//! no budget pressure, and instead strict ordering, operations that survive
//! being delivered twice, and a client that can disconnect mid-turn and rejoin.
//!
//! What it demonstrates is that one game can hold both and switch between them
//! at runtime, and that the switch is a room boundary rather than a mode flag.
//!
//! Nothing here borrows from any existing creature game: the creatures are
//! invented and so are their names.

pub mod battle;
pub mod grid;
pub mod protocol;
pub mod world;
#[cfg(feature = "server")]
pub mod state;
