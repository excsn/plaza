//! Standing the arena up behind a WebSocket, and serving the browser client
//! from the same port.
//!
//! [`plaza_session::host::SimHost`] is the whole stack (session with the
//! build's protocol and simulation clock, controller, fixed-step driver, the
//! `/ws` route, and the HTTP side with its cache busting). What is left here is
//! the part that is actually this arena's: which state, which logic, and at
//! what tick rate.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use plaza_session::host::SimHost;

use crate::net::arena::{Arena, ArenaLogic, HostView};
use crate::sim::protocol::PROTOCOL;
use crate::sim::types::{Controls, B0MB_SEED, SIM_STEP_MS};

/// The browser client artifact, the one asset that must never be served stale.
const WASM_FILE: &str = "bomb_grid.wasm";

/// Runs the arena until the process ends.
///
/// Blocks, so a windowed host calls it on a background thread with its own
/// runtime while the frame loop keeps the main thread.
pub async fn serve(bind: &str, controls: Arc<Mutex<Controls>>, view: Option<Arc<Mutex<HostView>>>, static_dir: Option<String>) -> std::io::Result<()> {
  let initial = *controls.lock();
  SimHost::new(bind, Duration::from_millis(SIM_STEP_MS))
    .serve_dir(static_dir)
    .cache_bust(WASM_FILE)
    // `run_fixed` underneath, never `run`: measured elapsed time would make the
    // simulation's rate a property of the host's scheduler, and on a lattice
    // that lands as a snap at every cell boundary.
    .run(plaza_wire::MsgPackCodec, PROTOCOL, Arena::new(initial, B0MB_SEED), |wiring| {
      ArenaLogic::new(controls, view).with_link(wiring.link_sink()).with_clock(wiring.sim_clock.clone())
    })
    .await
}
