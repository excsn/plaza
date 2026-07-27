//! Main entry point for the CSP Net Example.
//!
//! Runs an authoritative Plaza server and any number of predicting clients in a
//! single process, connected by MPSC channels that stand in for a real network.
//! That lets the example demonstrate client-side prediction and server
//! reconciliation without needing sockets.

mod client;
mod common_types;
mod server;

use plaza::agent::Agent;
use std::env;
use std::time::Duration;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  let subscriber = FmtSubscriber::builder()
    .with_max_level(Level::INFO)
    .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
    .with_timer(tracing_subscriber::fmt::time::uptime())
    .finish();
  tracing::subscriber::set_global_default(subscriber).expect("Setting default tracing subscriber failed.");

  let args: Vec<String> = env::args().collect();
  if args.len() > 1 && args[1] == "--help" {
    print_usage();
    return Ok(());
  }

  let num_clients: usize = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(2);
  let run_secs: u64 = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(5);

  tracing::info!(
    "Starting CSP demo: {} client(s), running for {}s.",
    num_clients,
    run_secs
  );

  let server = server::start_server().await?;

  let mut client_handles = Vec::new();
  for i in 0..num_clients {
    let client_name = format!("Client-{}", i + 1);
    let agent = Agent::new_human(Uuid::new_v4());
    let (to_server_tx, from_server_rx) = server.connect_client(agent)?;

    client_handles.push(tokio::spawn(async move {
      if let Err(e) = client::run_client(client_name.clone(), to_server_tx, from_server_rx).await {
        tracing::error!("[{}] exited with error: {}", client_name, e);
      }
    }));

    // Stagger joins slightly so the logs are readable.
    tokio::time::sleep(Duration::from_millis(100)).await;
  }

  tokio::time::sleep(Duration::from_secs(run_secs)).await;
  tracing::info!("Demo duration elapsed; shutting down.");

  for handle in &client_handles {
    handle.abort();
  }
  server.shutdown().await?;
  tracing::info!("CSP demo finished.");

  Ok(())
}

fn print_usage() {
  eprintln!("Plaza CSP Net Example: client-side prediction over a simulated network.");
  eprintln!("\nUsage:");
  eprintln!("  cargo run -p plaza_csp_net_example -- [num_clients] [run_seconds]");
  eprintln!("\nDefaults: 2 clients, 5 seconds.");
  eprintln!("\nExamples:");
  eprintln!("  cargo run -p plaza_csp_net_example");
  eprintln!("  cargo run -p plaza_csp_net_example -- 3 10");
}
