//! An interactive, in-browser demonstration of rollback netcode, built on
//! `plaza_client_utils`'s `rollback` module: deterministic-lockstep peers that
//! predict each other's inputs and roll back when a guess is disproved.
//!
//! Where the `netcode_playground` shows the server-authoritative model (predict
//! your own entity, let the server correct it), this shows the peer-to-peer one:
//! two peers run the same simulation, exchange only inputs, and stay identical.
//!
//! The library half is the headless [`sim`], which the binary renders with
//! macroquad. Splitting them keeps the simulation testable without a window.

pub mod sim;
