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
//!   last frame took into whole fixed steps, or into "is it time yet". Two
//!   simulations running the same rule at different step sizes are not the same
//!   simulation, and the drift reads as network jitter, so both sides taking the
//!   step from here is what keeps them equal.
//! - **`meter::RateMeter`**: what the wire cost, as a windowed rate rather than a
//!   session average that creeps for ever toward a level it never reaches.
//!   `plaza_server_utils` re-exports it, so both ends quote the same arithmetic.
//! - **`determinism`**: the draws, noise and hashes a shared rule derives its
//!   world from, identical on wasm and native and pinned by test, plus
//!   **`digest::StateDigest`** for hearing about a divergence before the screen
//!   shows it.
//! - **`rollback`**: the other netcode family, peer-to-peer deterministic lockstep.
//!   `StateHistory`, `InputTimeline`, and the `RollbackSession` bundle predict a
//!   missing remote input and roll back to re-simulate when the guess is disproved.
//!
//! # Four principles worth knowing before you predict or render anything
//!
//! None is enforceable by a type, and between them they account for every
//! netcode bug found while building the playground examples. They prevent
//! bugs, where everything else in this crate only recovers from them.
//! The first two are about simulation, the last two about rendering, and the
//! examples' `LEARNINGS.md` records what each one cost to learn.
//!
//! **1. A shared rule must be shared code, not code written twice.** The `apply`
//! you hand [`PredictedPlayer`] or [`HeldInputPredictor`] is meant to *be* the
//! server's step function, not a client approximation of it. Anything the server
//! does that your copy leaves out arrives as a permanent correction: it looks
//! like network jitter, it is largest exactly when it is most visible, and it is
//! extremely expensive to find later. Measured across two examples, every entity
//! whose rule lived in one function both sides called was correct and stayed
//! correct, and every entity whose rule was written twice drifted.
//!
//! If your client's rule needs the world to run (gravity, wind, a moving
//! platform), that is what the context parameter is for. Being unable to pass
//! the world in is exactly what pushes people into writing the second, lesser
//! rule, so it is a deficiency in the API rather than a reason to fork the rule.
//!
//! **2. Prediction is presentation; shared rules consume authoritative state.**
//! Feeding a locally predicted position into a rule that *both* sides run
//! creates a second, divergent world, and every packet then fights the local
//! one. Prediction drives the camera and the local player's own marker. The
//! rules both sides run read the authoritative state, even though it is older.
//! This is counterintuitive, because using the freshest local data looks like an
//! improvement.
//!
//! **3. One instant per frame.** A client that renders in the past picks a
//! single instant T for the whole frame, and everything is evaluated at T: not
//! only where entities are drawn, but everything a behaviour rule reads while
//! producing the frame, aim targets and chase context included. An entity
//! simulated to T while reading a target from the newest packet is two
//! timelines in one scene, and the seam between them is a bug whether or not
//! it is visible yet. [`interpolation::InterpolationClock`] supplies T; the
//! discipline of feeding *every* read from it is yours.
//!
//! **4. The timeline comes from declaration, not arrival.** Transport facts,
//! round trips and jitter and arrival times, may size buffers and admit or
//! refuse connections. They never decide which moment is on screen or when an
//! input executes; those are declared numbers the server chooses and
//! publishes. A render clock steered by packet arrival hides bad links instead
//! of reporting them, lets every client pick a different "now", and quietly
//! makes ping an input to the game.
//!
//! # The resume contract
//!
//! Every long-lived client eventually stops reading: a browser tab goes to the
//! background, a laptop sleeps, a frame loop stalls. The socket keeps
//! receiving the whole time, so what a resumed client faces is not a slow
//! stream but a *lump*: minutes of packets, delivered at once, describing
//! moments it can never play. The recovery that works is built from one
//! invariant, stated here because each half lives in a different crate:
//!
//! **A client may discard any stretch of the stream unread, provided it also
//! drops the state derived from it, because an acknowledgement carrying the
//! digest of nothing obligates the server to answer with a full baseline.**
//!
//! That is the digest-and-rebuild machinery of `server_utils::DeltaBaseline`
//! and [`mirror::DeltaMirror`], read as a permission. It is why there is no
//! "resync request" message anywhere: dropping the mirror *is* the request.
//! On top of it, resume is three verdicts at three layers, each owned by a
//! block:
//!
//! - the **transport** discards the backlog before parsing it
//!   (`plaza_ws::trim_backlog`), because none of it survives what follows;
//! - the **playout queue** treats the gap as a discontinuity and restarts
//!   once, keeping only the newest packet ([`PlayoutBuffer`]);
//! - the **server** stops streaming to a subscriber that has provably stopped
//!   reading (`DeltaBaseline::with_flow`), so the lump never grows to
//!   megabytes in the first place.
//!
//! The application's remaining job is small and cannot be taken from it: on
//! [`playout::Admission::TimelineLost`], drop the mirror and re-anchor the
//! render clock on what just arrived.
//!
//! # Which predictor
//!
//! The two differ by how the *server* consumes input, not by how the client
//! feels. Choosing wrong is silent, and shows up as a prediction that is always
//! slightly behind.
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
//! `plaza_client_utils` aims to provide foundational building blocks, not a complete
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
pub mod determinism;
pub mod meter;
pub mod mirror;
pub mod playout;
pub mod prediction;
pub mod predicted_player;
pub mod remote_view;
pub mod rollback;
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
pub use smoothing::AdaptiveDecay;
pub use rtt::RttEstimator;
pub use slot::{ReusePolicy, SlotAllocator, SlotKey};
pub use timeline::{Probe, Timeline};
pub use timestep::{FixedTimestep, Periodic, Steps};
pub use smoothing::{ease_in_cubic, ease_in_out_quad, ease_in_quad, ease_out_cubic, linear, smoothstep, Easing, ErrorSmoother};
pub use types::{ClientTimeMs, SequenceNumber};