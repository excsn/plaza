//! Standing the arena up behind a WebSocket, and serving the browser client
//! from the same port.
//!
//! [`plaza_session::host::SimHost`] is the whole stack (session with the
//! build's protocol and simulation clock, controller, fixed-step driver, the
//! `/ws` route, and the HTTP side with its cache busting). What is left here is
//! the part that is actually this arena's: which state, which logic, and where
//! the admission measurement comes from.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use plaza_session::host::SimHost;

use crate::net::arena::{Arena, ArenaLogic, HostView, PlayerKey, RttSource};
use crate::sim::protocol::PROTOCOL;
use crate::sim::types::{Controls, SIM_STEP_MS};

/// The browser client artifact, the one asset that must never be served stale.
const WASM_FILE: &str = "hit_scan.wasm";

/// Seeds spawn placement, so two hosts started with the same number run the
/// same fight.
const ARENA_SEED: u64 = 0x5C_0B_E5_EE;

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
    // simulation's rate a property of the host's scheduler, and here that would
    // land as a rewind reaching a tick that covered a different amount of time
    // on each machine.
    .run(plaza_wire::MsgPackCodec, PROTOCOL, Arena::new(initial, ARENA_SEED), |wiring| {
      // One way, from the round trip the *server* measured. A client's word
      // about its own latency is the one number worth lying about, and this
      // one decides who gets in.
      let rtt = {
        let session = wiring.session.clone();
        Arc::new(move |key: &PlayerKey| session.agent_rtt(key).map(|(rtt, _)| rtt.as_millis() as u64 / 2)) as RttSource
      };
      ArenaLogic::new(controls, view)
        .with_link(wiring.link_sink())
        .with_rtt(rtt)
        .with_clock(wiring.sim_clock.clone())
    })
    .await
}
