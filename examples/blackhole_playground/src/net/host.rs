//! Standing the arena up behind a WebSocket, and serving the browser client
//! from the same port.
//!
//! The whole stack is [`plaza_session::host::SimHost`], on
//! [`measured`](SimHost::measured) rather than the fixed-step default: this
//! sim integrates over elapsed time and clients absorb the difference as
//! corrections, which is the one shape that mode exists for. What is left
//! here is the part that is actually this arena's: which state, which logic,
//! and at what tick rate.

use std::sync::Arc;

use parking_lot::Mutex;
use plaza_session::host::SimHost;

use crate::net::arena::{Arena, ArenaLogic, HostView};
use crate::sim::protocol::PROTOCOL;
use crate::sim::types::Controls;

/// The tick rate the simulation is advanced at. Distinct from the *send* rate,
/// which is `Controls::sync_hz` and is usually far lower: simulating often and
/// sending rarely is the whole reason this example exists.
const TICK_HZ: u32 = 60;

/// The browser client artifact, the one asset that must never be served stale.
const WASM_FILE: &str = "blackhole_playground.wasm";

/// Runs the arena until the process ends.
///
/// Blocks, so a windowed host calls it on a background thread with its own
/// runtime while the frame loop keeps the main thread.
///
/// `controls` is shared with the host's UI, which is how the panel's sliders
/// reach a running arena; a headless server passes the fixed set it launched
/// with and nothing ever writes it. `view` is where the arena publishes its
/// omniscient state for a windowed host to read, and `None` for a headless one
/// that has no screen to draw it on.
pub async fn serve(bind: &str, controls: Arc<Mutex<Controls>>, view: Option<Arc<Mutex<HostView>>>, static_dir: Option<String>) -> std::io::Result<()> {
  let initial = *controls.lock();
  SimHost::measured(bind, TICK_HZ)
    .serve_dir(static_dir)
    // The wasm is a build product: it does not rebuild when the host does, so a
    // browser holding an older copy is the normal case rather than an exotic one.
    .cache_bust(WASM_FILE)
    .run(plaza_wire::MsgPackCodec, PROTOCOL, Arena::new(initial), |wiring| {
      ArenaLogic::new(controls, view)
        .with_link(wiring.link_sink())
        .with_clock(wiring.sim_clock.clone())
    })
    .await
}
