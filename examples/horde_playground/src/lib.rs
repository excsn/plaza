//! The many-entity case: a bullet-heaven horde, networked.
//!
//! Thousands of enemies, several players standing in different parts of a world
//! far larger than one screen, and a bandwidth budget. This is the example the
//! backlog kept deferring, and it exists to settle questions by measurement
//! rather than argument:
//!
//! - what per-player relevance actually saves, and whether it still pays when
//!   players cluster together instead of spreading out;
//! - whether a very low sync rate (1 Hz) is usable, and by which drawing
//!   strategy;
//! - whether running the enemy's *behaviour rule* locally beats interpolating
//!   between sparse samples, or dead-reckoning along a stale velocity;
//! - what compact entity ids and quantized positions are worth on the wire.
//!
//! It is built on `plaza_server_utils::relevance` for the interest management and
//! `plaza_client_utils` for the correction smoothing, over a real socket (the
//! `--role host`/`client` listen-server) or the deterministic `net_sim` link (the
//! offline teaching build).

pub mod net;
pub mod role;
pub mod sim;
