//! Moderation as a live tool, and the surface it needs.

pub mod client;
pub mod logic;
pub mod moderation;
pub mod snapshot;
pub mod types;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use plaza::controller::StateControllerBuilder;
use plaza::tick_driver::TickDriver;
use plaza_session::codec::JsonCodec;
use plaza_session::tcp::{AgentFactory, TcpPlazaSession};
use plaza_session::{Rate, SessionOptions};

use crate::logic::{PartyLogic, PartyState};
use crate::moderation::{steward, Host};
use crate::snapshot::TableSnapshotter;
use crate::types::{PartyOp, FLOOD_OPS, FLOOD_WINDOW_MS};

pub type Party = TcpPlazaSession<PartyOp, u64>;

/// The whole party: the shipped TCP transport with the host's tools reading
/// the manager. No transport is written here, which is the point.
pub async fn party(afk: Duration) -> (Arc<Party>, Arc<Host>) {
  let next_key = Arc::new(AtomicU64::new(1));
  let factory: AgentFactory<u64> = Arc::new(move |_peer| {
    Ok(plaza::agent::Agent::new_human(
      next_key.fetch_add(1, Ordering::Relaxed),
    ))
  });

  let session = Party::bind_with_options(
    "127.0.0.1:0",
    factory,
    JsonCodec,
    SessionOptions::default().rate_limit_inbound(
      Rate::per_second(FLOOD_OPS as f64 * 1000.0 / FLOOD_WINDOW_MS as f64).burst(FLOOD_OPS as u32),
    ),
  )
  .await
  .expect("bind");
  let host = Host::new(session.manager().clone());

  let (tx, controller) = StateControllerBuilder::new(
    Arc::new(PartyLogic {
      host: host.clone(),
      host_key: Default::default(),
    }),
    session.clone(),
    Arc::new(TableSnapshotter { host: host.clone() }),
    PartyState::default(),
  )
  .build();
  tokio::spawn(controller.run());
  tokio::spawn(TickDriver::new(Duration::from_millis(50)).run(tx));
  tokio::spawn(steward(host.clone(), afk));

  (session, host)
}
