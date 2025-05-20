//! Main entry point for the CSP Net Example.
//! Allows running as either a server or a client instance.

// Declare modules that will contain server and client logic, and shared types.
mod client;
mod common_types;
mod server;

use std::env;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  // Initialize tracing (logging)
  let subscriber = FmtSubscriber::builder()
    .with_max_level(Level::INFO) // Default to INFO, can be overridden by RUST_LOG
    .with_env_filter(tracing_subscriber::EnvFilter::from_default_env()) // Allow RUST_LOG
    .with_timer(tracing_subscriber::fmt::time::uptime()) // Show time since start
    .finish();
  tracing::subscriber::set_global_default(subscriber).expect("Setting default tracing subscriber failed.");

  let args: Vec<String> = env::args().collect();

  if args.len() < 2 {
    print_usage();
    return Ok(());
  }

  match args[1].as_str() {
    "server" => {
      tracing::info!("Starting CSP Net Example: SERVER Mode");
      // Placeholder for server arguments if needed later (e.g., port)
      // let server_args = &args[2..];
      if let Err(e) = server::run_server().await {
        tracing::error!("Server exited with error: {}", e);
        return Err(e.into()); // Convert PlazaError to Box<dyn Error>
      }
    }
    "client" => {
      tracing::info!("Starting CSP Net Example: CLIENT Mode");
      let num_clients = if args.len() > 2 {
        args[2].parse::<usize>().unwrap_or(1)
      } else {
        1 // Default to 1 client if not specified
      };
      let client_id_prefix = if args.len() > 3 {
        args[3].clone()
      } else {
        "Client".to_string() // Default prefix
      };

      tracing::info!(
        "Launching {} client(s) with prefix '{}'...",
        num_clients,
        client_id_prefix
      );

      let mut client_handles = Vec::new();

      for i in 0..num_clients {
        let client_name = format!("{}-{}", client_id_prefix, i + 1);
        // Spawn each client in its own Tokio task
        let handle = tokio::spawn(async move {
          tracing::info!("Client task {} starting...", client_name);
          if let Err(e) = client::run_client(client_name).await {
            tracing::error!("Client exited with error: {}", e);
          }
        });
        client_handles.push(handle);
        // Stagger client starts slightly to make logs easier to read (optional)
        if num_clients > 1 {
          tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
      }

      // Wait for all client tasks to complete
      for handle in client_handles {
        let _ = handle.await; // We don't care much about join errors for this example
      }
      tracing::info!("All client tasks finished.");
    }
    _ => {
      print_usage();
    }
  }

  Ok(())
}

fn print_usage() {
  eprintln!("Plaza CSP Net Example");
  eprintln!("Usage:");
  eprintln!("  cargo run --example csp_net_example -- server");
  eprintln!("  cargo run --example csp_net_example -- client [num_clients] [client_id_prefix]");
  eprintln!("\nArguments for client (optional):");
  eprintln!("  num_clients: Number of client instances to simulate (default: 1).");
  eprintln!("  client_id_prefix: Prefix for client identifiers (default: 'Client').");
  eprintln!("\nExamples:");
  eprintln!("  cargo run --example csp_net_example -- server");
  eprintln!("  cargo run --example csp_net_example -- client");
  eprintln!("  cargo run --example csp_net_example -- client 3 Bot");
}
