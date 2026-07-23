//! An interactive, in-browser demonstration of `plaza_client_utils`: client-side
//! prediction, server reconciliation, and entity interpolation, in the shape
//! Gabriel Gambetta's articles use.
//!
//! The library half is the headless [`sim`], which the binary renders with
//! macroquad. Splitting them keeps the simulation testable without a window.

pub mod sim;
