// src/main.rs
mod logic;
mod session;
mod snapshot;
mod types;

use crate::{
  logic::PongLogic,
  session::{ActixWsPongSession, ForwardOpFromWsTask, PlazaSessionOverActix, RegisterWsTask, WsTaskTerminated},
  snapshot::PongSnapshotter,
  types::{PlayerId, PongGameState, PongOp, PongSnapshotPayload},
};

use plaza::{agent::Agent, controller::StateControllerBuilder, session::SessionMessage};

use actix::Actor;
use actix_web::{rt, web, App, Error as ActixWebError, HttpRequest, HttpResponse, HttpServer};
use actix_ws::{AggregatedMessage, CloseCode, CloseReason, Message as WsMessage}; // Reverted to direct import
use futures_util::{StreamExt as _, TryStreamExt as _};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc as tokio_mpsc;
use tracing::{debug, error, info, warn, Level};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

fn start_plaza_session_components() -> (actix::Addr<ActixWsPongSession>, Arc<PlazaSessionOverActix>) {
  let (incoming_tx_bc, _) = tokio::sync::broadcast::channel(session::CHANNEL_CAPACITY);
  let (joined_tx_bc, _) = tokio::sync::broadcast::channel(session::CHANNEL_CAPACITY);
  let (left_tx_bc, _) = tokio::sync::broadcast::channel(session::CHANNEL_CAPACITY);

  let manager_addr = ActixWsPongSession::create({
    let incoming_tx_bc = incoming_tx_bc.clone();
    let joined_tx_bc = joined_tx_bc.clone();
    let left_tx_bc = left_tx_bc.clone();
    move |_ctx| ActixWsPongSession {
      next_conn_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
      active_tasks: Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new())),
      incoming_message_tx: incoming_tx_bc.clone(),
      agent_joined_tx: joined_tx_bc.clone(),
      agent_left_tx: left_tx_bc.clone(),
    }
  });

  let plaza_session_adapter = Arc::new(PlazaSessionOverActix {
    manager_addr: manager_addr.clone(),
    incoming_rx_template: incoming_tx_bc,
    joined_rx_template: joined_tx_bc,
    left_rx_template: left_tx_bc,
  });

  (manager_addr, plaza_session_adapter)
}

async fn ws_route(
  req: HttpRequest,
  stream: web::Payload,
  session_manager_addr: web::Data<actix::Addr<ActixWsPongSession>>,
) -> Result<HttpResponse, ActixWebError> {
  let player_id = Uuid::new_v4();
  let player_agent = Agent::new_human(player_id, format!("Player-{}", &player_id.to_string()[..8]));
  info!("WebSocket attempt from agent: {}", player_agent.label());

  let (response, mut ws_session_sender, ws_stream_receiver_raw) = actix_ws::handle(&req, stream).map_err(|e| {
    error!("WebSocket handshake error: {:?}", e);
    actix_web::error::ErrorInternalServerError("WebSocket handshake failed")
  })?;

  let manager_addr_clone = session_manager_addr.get_ref().clone();
  let player_agent_clone_for_task = player_agent.clone();

  rt::spawn(async move {
    let mut ws_stream_aggregated = ws_stream_receiver_raw
      .aggregate_continuations()
      .max_continuation_size(1024 * 1024);

    let (client_bound_tx_for_manager, mut client_bound_rx_for_task) =
      tokio_mpsc::channel::<SessionMessage<PongOp, PlayerId, PongSnapshotPayload>>(session::CHANNEL_CAPACITY / 4);

    let registration_result = manager_addr_clone
      .send(RegisterWsTask {
        player_agent: player_agent_clone_for_task.clone(),
        client_out_sender: client_bound_tx_for_manager,
      })
      .await;

    let conn_id = match registration_result {
      Ok(Ok((id, _dummy_rx))) => id,
      Ok(Err(e)) => {
        error!("Failed to register WebSocket task with manager: {}. Closing WS.", e);
        let _ = ws_session_sender.close(Some(CloseReason::from(CloseCode::Error))).await;
        return;
      }
      Err(e) => {
        error!("Mailbox error during WebSocket task registration: {}. Closing WS.", e);
        let _ = ws_session_sender.close(Some(CloseReason::from(CloseCode::Error))).await;
        return;
      }
    };

    info!(
      "WS task for player {} (conn_id {}) registered. Listening for messages.",
      player_agent_clone_for_task.label(),
      conn_id
    );

    loop {
      tokio::select! {
          biased;

          Some(msg_res) = ws_stream_aggregated.next() => {
              match msg_res {
                  Ok(AggregatedMessage::Text(text)) => {
                      debug!("Conn {}: WS RECV: {}", conn_id, text);
                      match serde_json::from_str::<PongOp>(&text) {
                          Ok(op) => {
                              if manager_addr_clone.try_send(ForwardOpFromWsTask {
                                  from_agent: player_agent_clone_for_task.clone(),
                                  op,
                              }).is_err() {
                                  warn!("Conn {}: Failed to forward op to manager (mailbox full/closed)", conn_id);
                              }
                          }
                          Err(e) => {
                              warn!("Conn {}: Failed to parse client op: {}. Raw: {}", conn_id, e, text);
                              let _ = ws_session_sender.text(format!("Error: Invalid JSON format: {}", e)).await;
                          }
                      }
                  }
                  Ok(AggregatedMessage::Ping(ping_data)) => {
                      if ws_session_sender.pong(&ping_data).await.is_err() {
                          warn!("Conn {}: Failed to send pong, client might be gone.", conn_id);
                          break;
                      }
                  }
                  Ok(AggregatedMessage::Pong(_pong_data)) => { // Added this arm
                      debug!("Conn {}: Received Pong from client.", conn_id);
                      // Usually, this means the client is responsive to our pings.
                      // The actix_ws::Session might handle heartbeat updates internally.
                      // No explicit action needed here unless custom logic is required.
                  }
                  Ok(AggregatedMessage::Close(reason)) => {
                      info!("Conn {}: WebSocket closed by client (aggregated): {:?}", conn_id, reason);
                      break;
                  }
                  Err(e) => {
                      warn!("Conn {}: WebSocket aggregated stream error: {:?}", conn_id, e);
                      break;
                  }
                  Ok(AggregatedMessage::Binary(_bin)) => {
                      warn!("Conn {}: Received unexpected binary message.", conn_id);
                  }
              }
          }

          Some(session_msg_to_send) = client_bound_rx_for_task.recv() => {
              match serde_json::to_string(&session_msg_to_send) {
                  Ok(json_str) => {
                      debug!("Conn {}: WS SEND: {}", conn_id, json_str);
                      if ws_session_sender.text(json_str).await.is_err() {
                          warn!("Conn {}: Failed to send message to WS client, client might be gone.", conn_id);
                          break;
                      }
                  }
                  Err(e) => {
                      error!("Conn {}: Failed to serialize server message for WS: {}", conn_id, e);
                  }
              }
          }

          else => {
              info!("Conn {}: Both WS stream and manager channel closed. Terminating task.", conn_id);
              break;
          }
      }
    }

    info!(
      "Conn {}: WebSocket task processing loop ended for player {}.",
      conn_id,
      player_agent_clone_for_task.label()
    );
    manager_addr_clone.do_send(WsTaskTerminated {
      conn_id,
      player_id: player_agent_clone_for_task.id().cloned().unwrap_or_else(Uuid::new_v4),
    });
    let _ = ws_session_sender
      .close(Some(CloseReason::from(CloseCode::Normal)))
      .await;
  });

  Ok(response)
}

async fn index() -> HttpResponse {
  HttpResponse::Ok()
    .content_type("text/html; charset=utf-8")
    .body(include_str!("../static/index.html"))
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
  tracing_subscriber::fmt()
    .with_max_level(Level::DEBUG)
    .with_env_filter(
      EnvFilter::from_default_env()
        .add_directive("info".parse().unwrap())
        .add_directive("plaza_example_pong=debug".parse().unwrap())
        .add_directive("plaza=debug".parse().unwrap())
        .add_directive("actix_ws=info".parse().unwrap()),
    )
    .init();

  info!("Plaza Pong Example - Starting (actix_ws::handle pattern)");

  let initial_state = PongGameState::default();
  let pong_logic = Arc::new(PongLogic::default());
  let pong_snapshotter = Arc::new(PongSnapshotter::default());

  let (session_manager_addr, plaza_session_adapter) = start_plaza_session_components();

  let (_controller_tx, controller) = StateControllerBuilder::new()
    .op_handler(pong_logic)
    .initial_state(initial_state)
    .session(plaza_session_adapter)
    .snapshot_provider(pong_snapshotter)
    .command_buffer(128)
    .build()
    .expect("Failed to build StateController");

  tokio::spawn(async move {
    info!("StateController task starting...");
    if let Err(e) = controller.run().await {
      error!("StateController exited with error: {}", e);
    }
    info!("StateController task finished.");
  });

  tokio::time::sleep(Duration::from_millis(100)).await;

  let server_addr = "127.0.0.1:8080";
  info!("Starting HTTP server and WebSocket endpoint at ws://{}", server_addr);

  HttpServer::new(move || {
    App::new()
      .app_data(web::Data::new(session_manager_addr.clone()))
      .route("/", web::get().to(index))
      .route("/ws", web::get().to(ws_route))
  })
  .bind(server_addr)?
  .run()
  .await
}
