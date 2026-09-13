//! `plaza_client_utils`
//!
//! This crate provides client-side utilities designed to complement applications
//! built with the Plaza server framework. It focuses on helping client applications
//! implement common networking patterns such as:
//!
//! - **Client-Side Prediction (CSP):** Allowing clients to predict the outcome of their
//!   inputs locally for immediate feedback.
//! - **Server Reconciliation:** Correcting client predictions with authoritative state
//!   received from the server.
//! - **State Interpolation/Extrapolation:** Smoothing the display of remote entities.
//!
//! The utilities are designed to be generic and unopinionated about the specific
//! game engine or rendering library used by the client application. They provide
//! data structures and algorithms that operate on application-defined `StateType`
//! and `ClientOp` types.
//!
//! # Core Components
//!
//! - **`input_buffer::ClientInputBuffer`**: Stores a history of client inputs sent to
//!   the server, essential for replaying inputs during reconciliation.
//! - **`prediction::PredictedEntity`**: Manages the predicted state of a client-controlled
//!   entity and handles the reconciliation process against server updates.
//! - **`interpolation::SnapshotBuffer`** and the **`Interpolatable`** trait: buffer server
//!   snapshots of remote entities and interpolate between them for smooth rendering.
//!   **`interpolation::InterpolationClock`** supplies the render-time target they need.
//! - **`extrapolation::ExtrapolationBase`** and the **`Extrapolatable`** trait: project a
//!   remote entity's movement for short durations to hide gaps between updates.
//! - **`smoothing::ErrorSmoother`**: eases a reconciliation correction over a few frames
//!   instead of snapping it.
//! - **`timestep::FixedTimestep`** and **`Periodic`**: turning however long the
//!   last frame took into whole fixed steps or into "is it time yet". Two
//!   simulations running the same rule at different step sizes drift apart and
//!   the drift reads as network jitter, so both sides should take the step from
//!   here.
//! - **`meter::RateMeter`**: what the wire cost, as a windowed rate. A session
//!   average would keep creeping toward a new level without reaching it.
//!   `plaza_server_utils` re-exports it, so both ends use the same arithmetic.
//! - **`determinism`**: the draws, noise and hashes a shared rule derives its
//!   world from, identical on wasm and native and pinned by test, plus
//!   **`digest::StateDigest`** for detecting a divergence before it shows on
//!   screen.
//! - **`rollback`**: the other netcode family, peer-to-peer deterministic lockstep.
//!   `StateHistory`, `InputTimeline` and the `RollbackSession` bundle predict a
//!   missing remote input and roll back to re-simulate when the guess is disproved.
//!
//! # Four principles
//!
//! No type can enforce these. Between them they account for every netcode bug
//! found while building the playground examples. The rest of this crate can
//! only recover from those bugs after they happen. The first two are about
//! simulation and the last two about rendering. The examples' `LEARNINGS.md`
//! records the bugs behind each one.
//!
//! **1. A shared rule must be shared code, not code written twice.** The `apply`
//! you hand [`PredictedPlayer`] or [`HeldInputPredictor`] should be the server's
//! own step function rather than a client approximation of it. Anything the
//! server does that your copy leaves out shows up as a constant correction. It
//! looks like network jitter, it is largest when it is most visible and it is
//! very hard to track down. Across two examples, every entity whose rule lived
//! in one function both sides called stayed correct and every entity whose rule
//! was written twice drifted.
//!
//! If your client's rule needs the world to run (gravity, wind, a moving
//! platform), pass it through the context parameter. Without a way to pass the
//! world in, people end up writing a second, simpler rule. Treat that as a gap
//! in the API to fix and keep the one rule.
//!
//! **2. Prediction is presentation; shared rules consume authoritative state.**
//! Feeding a locally predicted position into a rule that both sides run makes
//! the client compute a different world from the server's and every packet then
//! pulls it back. Prediction drives the camera and the local player's own
//! marker. The rules both sides run read the authoritative state, even though
//! it is older. This is easy to get wrong, because using the freshest local
//! data looks like an improvement.
//!
//! **3. One instant per frame.** A client that renders in the past picks a
//! single instant T for the whole frame and evaluates everything at T. That
//! covers where entities are drawn and also everything a behaviour rule reads
//! while producing the frame, such as aim targets and chase context. An entity
//! simulated to T that reads its target from the newest packet mixes two
//! timelines in one scene, which is a bug even before it becomes visible.
//! [`interpolation::InterpolationClock`] supplies T; making every read use it
//! is up to you.
//!
//! **4. The timeline comes from declaration, not arrival.** Transport facts,
//! round trips and jitter and arrival times, may size buffers and admit or
//! refuse connections. They never decide which moment is on screen or when an
//! input executes; those are declared numbers the server chooses and
//! publishes. A render clock steered by packet arrival hides bad links instead
//! of reporting them and lets every client pick a different "now". It also
//! lets each player's ping change what happens in the game.
//!
//! # The resume contract
//!
//! Every long-lived client eventually stops reading, for example when a
//! browser tab goes to the background, a laptop sleeps or a frame loop stalls.
//! The socket keeps receiving the whole time, so a resumed client gets a
//! *lump*: minutes of packets delivered at once, describing moments it can no
//! longer play. Recovery rests on one invariant, stated here because each half
//! lives in a different crate:
//!
//! **A client may discard any stretch of the stream unread, provided it also
//! drops the state derived from it, because an acknowledgement carrying the
//! digest of nothing obligates the server to answer with a full baseline.**
//!
//! The digest-and-rebuild machinery of `server_utils::DeltaBaseline` and
//! [`mirror::DeltaMirror`] makes that discard safe. There is no "resync
//! request" message; dropping the mirror acts as the request. Resume handling
//! is split across three layers:
//!
//! - the **transport** discards the backlog before parsing it
//!   (`plaza_ws::trim_backlog`), because all of it would be dropped anyway;
//! - the **playout queue** treats the gap as a discontinuity and restarts
//!   once, keeping only the newest packet ([`PlayoutBuffer`]);
//! - the **server** stops streaming to a subscriber that has provably stopped
//!   reading (`DeltaBaseline::with_flow`), so the lump never grows to
//!   megabytes in the first place.
//!
//! The application has one job left: on
//! [`playout::Admission::TimelineLost`], drop the mirror and re-anchor the
//! render clock on what just arrived.
//!
//! # Which predictor
//!
//! Pick between the two by how the *server* consumes input. A wrong choice
//! raises no error; it shows up as a prediction that is always slightly behind.
//!
//! | the server | use |
//! |---|---|
//! | consumes one input per simulation step | [`PredictedPlayer`] (replay unacknowledged inputs) |
//! | holds an input and integrates it every tick | [`HeldInputPredictor`] (dead reckon and ease) |
//!
//! Replaying inputs against a server of the second kind double counts, and gets
//! worse the more you economise on bandwidth, because one coalesced input can
//! cover a long stretch of simulation.
//!
//! # Philosophy
//!
//! `plaza_client_utils` provides building blocks rather than a complete
//! client-side framework. The application developer is responsible for:
//! - Defining their `StateType` and `ClientOp` types.
//! - Implementing the client-side game logic (how an `Op` affects `StateType`).
//! - Integrating with their chosen networking library (e.g., WebSockets, WebRTC, renet)
//!   to send `ClientOp`s and receive server state updates.
//! - Driving the rendering loop and using the predicted/interpolated states.

pub mod ack;
pub mod arrival;
pub mod clock_sync;
pub mod coalesce;
pub mod correction;
pub mod digest;
pub mod error;
#[cfg(feature = "fixed")]
pub mod fixed;
pub mod filter;
pub mod held_input;
pub mod input_buffer;
pub mod absence;
pub mod determinism;
pub mod meter;
pub mod mirror;
pub mod playout;
pub mod prediction;
pub mod predicted_player;
pub mod remote_view;
pub mod rollback;
pub mod route;
pub mod slot;
pub mod types;
pub mod interpolation;
pub mod extrapolation;
pub mod hermite;
pub mod smoothing;
pub mod timeline;
pub mod timestep;
pub mod trajectory;
pub mod rtt;
pub mod math;

#[cfg(feature = "net-sim")]
pub mod net_sim;

pub use absence::Silence;
pub use ack::AckWindow;
pub use arrival::ArrivalMonitor;
pub use clock_sync::ClockSyncEstimator;
pub use coalesce::InputCoalescer;
pub use correction::{Correction, CorrectionMonitor};
pub use determinism::{mix64, ValueNoise, XorShift};
pub use digest::{SetDigest, StateDigest};
pub use error::ClientUtilError;
pub use held_input::{HeldInputConfig, HeldInputPredictor};
pub use hermite::{hermite_scalar, HermiteInterpolatable, HermiteView};
pub use filter::ScalarKalman;
pub use input_buffer::{BufferedInput, ClientInputBuffer};
pub use meter::RateMeter;
pub use mirror::{Agreement, DeltaMirror, Divergence};
pub use interpolation::{InterpolationClock, SnapshotBuffer};
pub use playout::{Admission, PlayoutBuffer};
pub use predicted_player::{PlayerConfig, PredictedPlayer};
pub use prediction::PredictedEntity;
pub use remote_view::{RemoteView, RenderOpts};
pub use rollback::{repeat_last_input, Frame, InputTimeline, RollbackConfig, RollbackSession, StateHistory};
pub use route::{Heard, RoutePredictor};
pub use smoothing::AdaptiveDecay;
pub use rtt::RttEstimator;
pub use slot::{ReusePolicy, SlotAllocator, SlotKey};
pub use timeline::{Probe, Timeline};
pub use timestep::{FixedTimestep, Periodic, Steps};
pub use smoothing::{ease_in_cubic, ease_in_out_quad, ease_in_quad, ease_out_cubic, linear, smoothstep, Easing, ErrorSmoother};
pub use types::{ClientTimeMs, SequenceNumber};