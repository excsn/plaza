//! Plaza, a server-authoritative foundation for real-time shared state.
//!
//! An application defines three things and Plaza runs the loop around them:
//!
//! - a **state type**: whatever data is being shared,
//! - an **op type**: the discrete operations that may change it,
//! - a [`StateLogic`], the rules mapping ops onto state.
//!
//! A [`StateController`] owns that state and is
//! the only thing that mutates it, processing inputs one at a time in its own
//! task. Because it is a single actor, application logic needs no locking. It
//! reaches clients through a [`Session`] and hands joining
//! clients a snapshot from a [`SnapshotProvider`].
//!
//! ```ignore
//! let session = InProcessSession::<MyOp, MyId, MySnapshot>::new();
//! let (tx, controller) = StateControllerBuilder::new(
//!     Arc::new(MyLogic), session.clone(), Arc::new(MySnapshotter), MyState::default(),
//!   ).build();
//! tokio::spawn(controller.run());
//! tokio::spawn(TickDriver::from_hz(60).run(tx.clone()));
//! ```
//!
//! For real networking use the transports in `plaza_session`; for rooms and
//! matchmaking, `plaza_lobby`.
//!
//! # Optional building blocks
//!
//! Beyond the core loop this crate ships pieces real-time apps tend to need:
//! schedulers ([`common::scheduler`]), finite state machines ([`common::fsm`]),
//! participant tracking ([`common::participants`]), turn/round/phase control
//! and scorekeeping ([`game_common`]), client prediction and lag compensation
//! ([`game_common::reconciliation`]), and op payloads for collaborative apps:
//! locking, presence, ordered collections ([`app_common`]). Use what fits;
//! none of it is required.

pub mod agent;
pub mod app_common;
pub mod common;
pub mod controller;
pub mod error;
pub mod game_common;
pub mod session;
pub mod snapshot;
pub mod state_logic;
pub mod stats;
pub mod tick_driver;

pub use agent::{Agent, AgentId};
pub use controller::{query_state, CommandSender, ControllerCommand, StateController, StateControllerBuilder};
pub use error::PlazaError;
pub use session::{InProcessSession, MessageTarget, Session, SessionMessage, TargetedOp};
pub use snapshot::{SnapshotData, SnapshotProvider};
pub use state_logic::{LogicInput, StateLogic};
pub use tick_driver::TickDriver;
