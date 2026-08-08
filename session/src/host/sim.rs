//! Everything between "I have a `StateLogic`" and "it is listening".
//!
//! Every listen-server example stood up the same stack by hand: a session
//! speaking a named codec with the build's protocol version and a simulation
//! clock for its pongs, a controller over the logic, a fixed-step driver, a
//! WebSocket route numbering its connections, and a [`Host`] serving the
//! browser client next to it. [`SimHost`] is that stack written once.
//!
//! It is a prescription built from blocks, and every choice in it is one the
//! blocks let you unmake by using them directly:
//!
//! - **Joiners get no snapshot.** This stack is for worlds streamed as deltas
//!   on a cadence, where a joiner is caught up by the stream itself. A world
//!   that catches joiners up with state wants `StateControllerBuilder` and its
//!   join snapshot instead.
//! - **Connections are numbered, and the number is the agent id.** Assigned at
//!   accept, never client-supplied. An application with identity has its own
//!   id type and registers its own route on a plain [`Host`].
//! - **The driver is `run_fixed` by default**: delivering measured elapsed
//!   time would make the simulation's rate a property of the host's scheduler,
//!   which no predicting, replaying or rolling-back client can reproduce.
//!   [`SimHost::measured`] is the one deliberate exception, for logic that
//!   only integrates over elapsed time; `TickDriver::run` documents the line.

use std::fmt::Debug;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use actix_web::{web, HttpRequest, HttpResponse};
use plaza::state_logic::StateLogic;
use plaza::{Agent, NoSnapshots, StateControllerBuilder, TickDriver};
use plaza_wire::frame::ProtocolVersion;
use plaza_wire::WireCodec;
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::actix_ws::ActixWsPlazaSession;
use crate::conditioner::LinkSink;
use crate::manager::SessionOptions;

use super::{init_logging, Host};

/// How often the driver wakes by default. Higher than any sane step rate on
/// purpose: waking more often than you step keeps the phase error small,
/// because a step is spent nearer the moment it was earned.
pub const DEFAULT_WAKE_HZ: u32 = 120;

/// The command channel depth the examples settled on.
pub const DEFAULT_COMMAND_BUFFER: usize = 256;

/// What the stack hands the application while the logic is being built: the
/// session, for wiring measurement sources and sinks into the logic, and the
/// slot the simulation publishes its clock into.
pub struct SimWiring<Op, C>
where
  Op: Serialize + DeserializeOwned + Clone + Debug + Send + Sync + 'static,
  C: WireCodec,
{
  pub session: Arc<ActixWsPlazaSession<Op, u64, C>>,
  /// Store the simulation's clock here each tick and the session stamps its
  /// pongs with it, so clients synchronise against simulation time rather than
  /// wall time.
  pub sim_clock: Arc<AtomicU64>,
}

impl<Op, C> SimWiring<Op, C>
where
  Op: Serialize + DeserializeOwned + Clone + Debug + Send + Sync + 'static,
  C: WireCodec,
{
  /// A [`LinkSink`] that applies a profile to every connection: the usual
  /// destination for a panel's impairment sliders.
  pub fn link_sink(&self) -> LinkSink {
    let session = self.session.clone();
    Arc::new(move |profile| session.set_all_link_profiles(profile))
  }
}

/// A [`Host`] with the simulation stack behind it.
///
/// ```no_run
/// # use std::time::Duration;
/// # use plaza_session::host::SimHost;
/// # async fn demo(logic: (), controls: ()) -> std::io::Result<()> {
/// # /*
/// SimHost::new("0.0.0.0:8080", Duration::from_millis(SIM_STEP_MS))
///   .serve_dir(static_dir)
///   .cache_bust("my_game.wasm")
///   .run(MsgPackCodec, PROTOCOL, Arena::new(initial), |wiring| {
///     ArenaLogic::new(controls, view)
///       .with_link(wiring.link_sink())
///       .with_clock(wiring.sim_clock.clone())
///   })
///   .await
/// # */ Ok(())
/// # }
/// ```
pub struct SimHost {
  host: Host,
  stepping: Stepping,
  wake_hz: u32,
  command_buffer: usize,
}

/// How the driver advances the simulation.
#[derive(Clone, Copy, Debug)]
enum Stepping {
  /// Whole steps of exactly this duration, whatever the scheduler did.
  Fixed(Duration),
  /// Measured elapsed time, delivered at the wake cadence.
  Measured,
}

impl SimHost {
  /// `step` is the simulation's step, the unit its ticks are counted in.
  pub fn new(bind: impl Into<String>, step: Duration) -> Self {
    Self {
      host: Host::new(bind),
      stepping: Stepping::Fixed(step),
      wake_hz: DEFAULT_WAKE_HZ,
      command_buffer: DEFAULT_COMMAND_BUFFER,
    }
  }

  /// The stack on measured elapsed time instead of fixed steps: each tick's
  /// `delta_time` is what the scheduler actually delivered, at `tick_hz`.
  ///
  /// For logic that only integrates over elapsed time, where being late means
  /// integrating a larger dt rather than falling behind; corrections-based
  /// prediction is the standing example. Anything a client predicts, replays
  /// or rolls back needs [`new`](Self::new), because no client can reproduce
  /// a step size the host's scheduler chose (`TickDriver::run` documents the
  /// failure modes).
  pub fn measured(bind: impl Into<String>, tick_hz: u32) -> Self {
    Self {
      host: Host::new(bind),
      stepping: Stepping::Measured,
      wake_hz: tick_hz,
      command_buffer: DEFAULT_COMMAND_BUFFER,
    }
  }

  /// See [`Host::serve_dir`].
  pub fn serve_dir(mut self, dir: Option<String>) -> Self {
    self.host = self.host.serve_dir(dir);
    self
  }

  /// See [`Host::cache_bust`].
  pub fn cache_bust(mut self, asset: impl Into<String>) -> Self {
    self.host = self.host.cache_bust(asset);
    self
  }

  /// See [`Host::announce`].
  pub fn announce(mut self, announce: bool) -> Self {
    self.host = self.host.announce(announce);
    self
  }

  /// How often the driver wakes (default [`DEFAULT_WAKE_HZ`], or `tick_hz`
  /// under [`measured`](Self::measured)). Under [`new`](Self::new) the *step*
  /// stays what was given there whatever this is set to; under `measured` this
  /// is the tick cadence itself.
  pub fn wake_hz(mut self, hz: u32) -> Self {
    self.wake_hz = hz;
    self
  }

  pub fn command_buffer(mut self, size: usize) -> Self {
    self.command_buffer = size;
    self
  }

  /// Stands the stack up and serves until the process ends.
  ///
  /// `protocol` is the build's wire format number; a stale browser bundle is
  /// told to reload by the handshake rather than half-working. `logic_for`
  /// receives the [`SimWiring`] so measurement sources and sinks can be wired
  /// into the logic it returns.
  ///
  /// Blocks, so a windowed host calls it on a background thread with its own
  /// runtime while the frame loop keeps the main thread.
  pub async fn run<C, Op, S, L, F>(self, codec: C, protocol: u32, initial: S, logic_for: F) -> std::io::Result<()>
  where
    C: WireCodec,
    Op: Serialize + DeserializeOwned + Clone + Debug + Send + Sync + 'static,
    S: Debug + Send + Sync + 'static,
    L: StateLogic<Op, u64, S>,
    F: FnOnce(&SimWiring<Op, C>) -> L,
  {
    init_logging();

    let sim_clock = Arc::new(AtomicU64::new(0));
    let session: Arc<ActixWsPlazaSession<Op, u64, C>> = ActixWsPlazaSession::with_options(
      codec,
      SessionOptions::with_protocol(ProtocolVersion(protocol)).clock({
        let sim_clock = sim_clock.clone();
        move || sim_clock.load(Ordering::Relaxed)
      }),
    );

    let wiring = SimWiring {
      session: session.clone(),
      sim_clock,
    };
    let logic = logic_for(&wiring);

    let (commands, controller) = StateControllerBuilder::new(Arc::new(logic), session.clone(), Arc::new(NoSnapshots), initial)
      .snapshot_context_on_join(None)
      .command_buffer(self.command_buffer)
      .build();
    tokio::spawn(controller.run());
    match self.stepping {
      Stepping::Fixed(step) => {
        tracing::info!(wake_hz = self.wake_hz, step_ms = step.as_millis() as u64, "sim host starting");
        tokio::spawn(TickDriver::from_hz(self.wake_hz).run_fixed(commands, step));
      }
      Stepping::Measured => {
        tracing::info!(tick_hz = self.wake_hz, "sim host starting on measured time");
        tokio::spawn(TickDriver::from_hz(self.wake_hz).run(commands));
      }
    }

    let route_state = web::Data::new(RouteState {
      session,
      next_key: AtomicU64::new(1),
    });
    self
      .host
      .run(move |cfg| {
        cfg.app_data(route_state.clone()).route("/ws", web::get().to(ws_route::<Op, C>));
      })
      .await
  }
}

struct RouteState<Op, C>
where
  Op: Serialize + DeserializeOwned + Clone + Debug + Send + Sync + 'static,
  C: WireCodec,
{
  session: Arc<ActixWsPlazaSession<Op, u64, C>>,
  next_key: AtomicU64,
}

async fn ws_route<Op, C>(
  req: HttpRequest,
  stream: web::Payload,
  state: web::Data<RouteState<Op, C>>,
) -> Result<HttpResponse, actix_web::Error>
where
  Op: Serialize + DeserializeOwned + Clone + Debug + Send + Sync + 'static,
  C: WireCodec,
{
  let key = state.next_key.fetch_add(1, Ordering::Relaxed);
  state.session.handle_connection(&req, stream, Agent::new_human(key))
}
