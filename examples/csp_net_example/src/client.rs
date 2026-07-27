//! Client-side logic for the CSP Net Example.
//! This simulates a client connecting to the server (via MPSC channels),
//! performing client-side prediction, server reconciliation, and interpolating
//! remote entities.

use crate::common_types::{BoxState, CspSnapshotPayload, GameOp, MoveInput, PlayerId, ServerTick, Vec2};
use plaza_client_utils::{
  input_buffer::ClientInputBuffer,
  interpolation::{Interpolatable, SnapshotBuffer},
  prediction::PredictedEntity,
  types::{ClientTimeMs, SequenceNumber},
};
use plaza::{
  agent::Agent,
  game_common::reconciliation::op_payloads::SequencedClientInput,
  session::SessionMessage,
};

use std::collections::HashMap;
use std::time::{Duration, Instant};
use fibre::mpsc;
use tracing::{debug, error, info, warn};

const CLIENT_TICK_RATE_HZ: u64 = 60;
const CLIENT_TICK_INTERVAL: Duration = Duration::from_millis(1000 / CLIENT_TICK_RATE_HZ);
const INPUT_SEND_INTERVAL_TICKS: u64 = 2; // Send input packet every 2 client ticks (~30hz)
const CLIENT_INPUT_BUFFER_SIZE: usize = 128;
const REMOTE_SNAPSHOT_BUFFER_SIZE: usize = 10;
const INTERPOLATION_DELAY_MS: u64 = 100; // Render remote entities 100ms in the past

struct ClientApp {
  client_name: String,
  my_player_id: Option<PlayerId>,
  server_tick_on_join_ack: ServerTick,

  predicted_box: Option<PredictedEntity<BoxState, MoveInput>>,
  input_buffer: ClientInputBuffer<MoveInput, BoxState>,
  next_input_seq: SequenceNumber,

  remote_boxes: HashMap<PlayerId, RemoteClientBox>,

  to_server_tx: mpsc::BoundedAsyncSender<SessionMessage<GameOp, PlayerId, CspSnapshotPayload>>,
  from_server_rx: mpsc::BoundedAsyncReceiver<SessionMessage<GameOp, PlayerId, CspSnapshotPayload>>,

  last_input_send_tick: u64,
  client_tick_counter: u64,
  client_current_time_ms: ClientTimeMs,
}

struct RemoteClientBox {
  current_display_state: BoxState,
  snapshot_buffer: SnapshotBuffer<ServerTick, BoxState>,
}

// ServerTick is u64, for which plaza_client_utils already provides `ToF32`,
// so SnapshotBuffer's interpolation factor works out of the box.

impl Interpolatable<ServerTick> for BoxState {
  fn interpolate(&self, other: &Self, t: f32, _time_a: ServerTick, _time_b: ServerTick) -> Self {
    BoxState {
      position: self.position.interpolate(&other.position, t, _time_a, _time_b),
      velocity: self.velocity.interpolate(&other.velocity, t, _time_a, _time_b),
    }
  }
}

impl ClientApp {
  async fn new(
    client_name: String,
    to_server_tx: mpsc::BoundedAsyncSender<SessionMessage<GameOp, PlayerId, CspSnapshotPayload>>,
    from_server_rx: mpsc::BoundedAsyncReceiver<SessionMessage<GameOp, PlayerId, CspSnapshotPayload>>,
  ) -> Self {
    Self {
      client_name,
      my_player_id: None,
      server_tick_on_join_ack: 0,
      predicted_box: None,
      input_buffer: ClientInputBuffer::new(CLIENT_INPUT_BUFFER_SIZE),
      next_input_seq: 1,
      remote_boxes: HashMap::new(),
      to_server_tx,
      from_server_rx,
      last_input_send_tick: 0,
      client_tick_counter: 0,
      client_current_time_ms: 0,
    }
  }

  fn apply_move_to_box_state(state: &mut BoxState, input: &MoveInput) {
    let move_vec = input_to_velocity_vector(input);

    // Client predicts movement based on its own tick interval
    let effective_speed = super::server::MAX_PLAYER_SPEED / CLIENT_TICK_RATE_HZ as f32;

    state.position.x += move_vec.x * effective_speed;
    state.position.y += move_vec.y * effective_speed;

    state.velocity = Vec2 {
      x: move_vec.x * super::server::MAX_PLAYER_SPEED,
      y: move_vec.y * super::server::MAX_PLAYER_SPEED,
    };

    state.position.x = state
      .position
      .x
      .clamp(super::server::WORLD_BOUNDS_X.0, super::server::WORLD_BOUNDS_X.1);
    state.position.y = state
      .position
      .y
      .clamp(super::server::WORLD_BOUNDS_Y.0, super::server::WORLD_BOUNDS_Y.1);
  }


  async fn run_loop(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut tick_timer = tokio::time::interval(CLIENT_TICK_INTERVAL);
    let start_time = Instant::now();

    info!("[{}] Sending Join Request", self.client_name);
    let join_op = GameOp::CS_RequestJoin;
    let temp_dummy_id_for_sending = PlayerId::new_v4();
    let agent_for_sending = Agent::new_human(temp_dummy_id_for_sending, self.client_name.clone());

    self
      .to_server_tx
      .send(SessionMessage::Ops {
        from: agent_for_sending.clone(), // Server will use this to associate connection
        ops: vec![join_op],
      })
      .await?;

    loop {
      tokio::select! {
          _ = tick_timer.tick() => {
              self.client_tick_counter += 1;
              self.client_current_time_ms = start_time.elapsed().as_millis() as ClientTimeMs;

              if self.my_player_id.is_some() && self.predicted_box.is_some() {
                  // Simulate some input every few ticks
                  if self.client_tick_counter % 5 == 0 {
                      let dx = if self.client_tick_counter % 10 == 0 { -5.0 } else { 5.0 };
                      let local_input = MoveInput { dx, dy: 0.0 };
                      self.next_input_seq += 1;

                      info!(
                          "[{}] Tick: {}, ClientTime: {}ms, Input (Seq {}): {:?}",
                          self.client_name, self.client_tick_counter, self.client_current_time_ms, self.next_input_seq, local_input
                      );

                      if let Some(pb) = self.predicted_box.as_mut() {
                          pb.apply_local_input_and_predict(
                              &local_input,
                              self.next_input_seq,
                              &mut self.input_buffer,
                              &Self::apply_move_to_box_state,
                          );
                      }

                      if self.client_tick_counter - self.last_input_send_tick >= INPUT_SEND_INTERVAL_TICKS {
                          let op_to_send = GameOp::CS_PlayerInput(SequencedClientInput {
                              sequence_number: self.next_input_seq,
                              input_data: local_input,
                          });
                          info!("[{}] Sending to Server: Op Seq {}", self.client_name, self.next_input_seq);
                          if self.to_server_tx.send(SessionMessage::Ops { from: agent_for_sending.clone(), ops: vec![op_to_send] }).await.is_err() {
                              error!("[{}] Failed to send input to server. Server down?", self.client_name);
                              return Ok(());
                          }
                          self.last_input_send_tick = self.client_tick_counter;
                      }
                  }
              }

              for (id, remote_box) in self.remote_boxes.iter_mut() {
                        let estimated_current_server_tick = self.server_tick_on_join_ack.saturating_add(
                            self.client_current_time_ms / super::server::SERVER_TICK_INTERVAL_MS
                        );
                        let target_interp_server_tick = estimated_current_server_tick.saturating_sub(
                            INTERPOLATION_DELAY_MS / super::server::SERVER_TICK_INTERVAL_MS
                        );

                        if let Some(interp_state) = remote_box.snapshot_buffer.get_interpolated_state(target_interp_server_tick) {
                            remote_box.current_display_state = interp_state;
                        } else {
                            // Buffer empty: keep showing the last known state.
                            tracing::trace!("[{}] Remote box {}: Snapshot buffer empty, cannot get interpolated state.", self.client_name, id);
                        }
                    }

              self.render_state();
          }

          Ok(server_msg_package) = self.from_server_rx.recv() => {
              match server_msg_package {
                  SessionMessage::Ops { from: _, ops } => {
                      for op in ops {
                          self.handle_server_op(op);
                      }
                  }
                  SessionMessage::StateData { from: _, data: _ } => {
                      // This server sends initial state as an SC_JoinAck op instead.
                      warn!("[{}] Received StateData, not handled in this simple client.", self.client_name);
                  }
              }
          }
          else => {
              info!("[{}] Server connection closed or error. Exiting.", self.client_name);
              break;
          }
      }
    }
    Ok(())
  }

  fn handle_server_op(&mut self, op: GameOp) {
    match op {
      GameOp::SC_JoinAck {
        your_id,
        initial_boxes,
        server_tick,
      } => {
        info!(
          "[{}] Joined game! My ID: {}. Server tick: {}. Initial boxes: {}",
          self.client_name,
          your_id,
          server_tick,
          initial_boxes.len()
        );
        self.my_player_id = Some(your_id);
        self.server_tick_on_join_ack = server_tick;
        self.client_current_time_ms = 0; // Reset client perception of time relative to server join tick.

        for (id, box_state) in initial_boxes {
          if Some(id) == self.my_player_id {
            self.predicted_box = Some(PredictedEntity::<BoxState, MoveInput>::new(box_state));
          } else {
            let mut sb = SnapshotBuffer::new(REMOTE_SNAPSHOT_BUFFER_SIZE);
            sb.add_snapshot(server_tick, box_state);
            self.remote_boxes.insert(
              id,
              RemoteClientBox {
                current_display_state: box_state,
                snapshot_buffer: sb,
              },
            );
          }
        }
        if let Some(my_id) = self.my_player_id {
          if let Some(my_box_initial_state) = self.predicted_box.as_ref().map(|pb| pb.last_authoritative_state) {
            self.predicted_box = Some(PredictedEntity::<BoxState, MoveInput>::new(my_box_initial_state));
            self.input_buffer = ClientInputBuffer::<MoveInput, BoxState>::new(CLIENT_INPUT_BUFFER_SIZE);
          } else {
            error!(
              "[{}] My ID {} was acked but no initial state found in SC_JoinAck.",
              self.client_name, my_id
            );
          }
        }
      }
      GameOp::SC_PlayerJoined {
        player_id,
        initial_state,
        server_tick,
      } => {
        if Some(player_id) != self.my_player_id {
          info!(
            "[{}] Remote player {} joined at server tick {}",
            self.client_name, player_id, server_tick
          );
          let mut sb = SnapshotBuffer::new(REMOTE_SNAPSHOT_BUFFER_SIZE);
          sb.add_snapshot(server_tick, initial_state);
          self.remote_boxes.insert(
            player_id,
            RemoteClientBox {
              current_display_state: initial_state,
              snapshot_buffer: sb,
            },
          );
        }
      }
      GameOp::SC_PlayerLeft { player_id } => {
        if Some(player_id) != self.my_player_id {
          info!("[{}] Remote player {} left", self.client_name, player_id);
          self.remote_boxes.remove(&player_id);
        } else {
          warn!(
            "[{}] Received PlayerLeft for myself. Server might have disconnected me.",
            self.client_name
          );
        }
      }
      GameOp::SC_AuthoritativeState(auth_update) => {
        if self.my_player_id.is_some() && self.predicted_box.is_some() {
          let pb = self.predicted_box.as_mut().unwrap();
          info!(
            "[{}] Received authoritative state for my box. Server Ack Seq: {}. Server Tick: {}. State: {:?}",
            self.client_name,
            auth_update.last_processed_input_seq,
            auth_update.server_time_at_state,
            auth_update.authoritative_player_state
          );
          pb.reconcile_with_server_state(
            auth_update.authoritative_player_state,
            auth_update.last_processed_input_seq,
            &mut self.input_buffer,
            &Self::apply_move_to_box_state,
          );
        }
      }
      GameOp::SC_RemoteEntitiesUpdate(snapshots) => {
        for remote_snapshot_data in snapshots {
          if Some(remote_snapshot_data.entity_id) != self.my_player_id {
            if let Some(remote_box) = self.remote_boxes.get_mut(&remote_snapshot_data.entity_id) {
              debug!(
                "[{}] Received remote update for {}: Pos {:?} @ ServerTick {}",
                self.client_name,
                remote_snapshot_data.entity_id,
                remote_snapshot_data.position,
                remote_snapshot_data.server_time
              );
              remote_box.snapshot_buffer.add_snapshot(
                remote_snapshot_data.server_time,
                BoxState {
                  position: remote_snapshot_data.position,
                  velocity: remote_snapshot_data.linear_velocity.unwrap_or_default(),
                },
              );
            } else {
              warn!(
                "[{}] Received update for unknown remote entity {}. Adding.",
                self.client_name, remote_snapshot_data.entity_id
              );
              let mut sb = SnapshotBuffer::new(REMOTE_SNAPSHOT_BUFFER_SIZE);
              sb.add_snapshot(
                remote_snapshot_data.server_time,
                BoxState {
                  position: remote_snapshot_data.position,
                  velocity: remote_snapshot_data.linear_velocity.unwrap_or_default(),
                },
              );
              self.remote_boxes.insert(
                remote_snapshot_data.entity_id,
                RemoteClientBox {
                  current_display_state: BoxState {
                    position: remote_snapshot_data.position,
                    velocity: remote_snapshot_data.linear_velocity.unwrap_or_default(),
                  },
                  snapshot_buffer: sb,
                },
              );
            }
          }
        }
      }
      // Client should not receive these
      GameOp::CS_PlayerInput(_) => {}
      GameOp::CS_RequestJoin => {}
    }
  }

  fn render_state(&self) {
    if self.my_player_id.is_none() {
      info!("[{}] Waiting for Join ACK from server...", self.client_name);
      return;
    }

    let mut render_output = format!(
      "[{}] Client Tick: {}, Client Time: {}ms\n",
      self.client_name, self.client_tick_counter, self.client_current_time_ms
    );
    if let Some(pb) = &self.predicted_box {
      render_output += &format!(
                "  My Box (ID: {:?} Pred): Pos=({:.1}, {:.1}), Vel=({:.1}, {:.1}) | Auth Last Known: Pos=({:.1}, {:.1}) @ ServAckSeq {}\n",
                self.my_player_id.unwrap(),
                pb.current_predicted_state.position.x, pb.current_predicted_state.position.y,
                pb.current_predicted_state.velocity.x, pb.current_predicted_state.velocity.y,
                pb.last_authoritative_state.position.x, pb.last_authoritative_state.position.y,
                pb.last_server_acknowledged_input_seq
            );
    }
    for (id, remote_box) in &self.remote_boxes {
      render_output += &format!(
        "  Remote Box (ID: {:?} Interp): Pos=({:.1}, {:.1}) | LastServerTickInBuf: {:?}\n",
        id,
        remote_box.current_display_state.position.x,
        remote_box.current_display_state.position.y,
        remote_box.snapshot_buffer.latest_timestamp().unwrap_or(0)
      );
    }
    if self.my_player_id.is_some() && !self.remote_boxes.is_empty() || self.predicted_box.is_some() {
      info!("{}", render_output.trim_end());
    }
  }
}

fn input_to_velocity_vector(input: &MoveInput) -> Vec2 {
  let mut move_vec = Vec2 {
    x: input.dx,
    y: input.dy,
  };
  let mag_sq = move_vec.x * move_vec.x + move_vec.y * move_vec.y;
  if mag_sq > 1.0 {
    // Normalize if input magnitude is > 1 (e.g. joystick full tilt)
    let mag = mag_sq.sqrt();
    if mag > f32::EPSILON {
      move_vec.x /= mag;
      move_vec.y /= mag;
    } else {
      return Vec2::default();
    }
  }
  move_vec
}

pub async fn run_client(
  client_name: String,
  to_server_tx: mpsc::BoundedAsyncSender<SessionMessage<GameOp, PlayerId, CspSnapshotPayload>>,
  from_server_rx: mpsc::BoundedAsyncReceiver<SessionMessage<GameOp, PlayerId, CspSnapshotPayload>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
  info!("[{}] Client task starting.", client_name);
  let mut app = ClientApp::new(client_name.clone(), to_server_tx, from_server_rx).await;

  if let Err(e) = app.run_loop().await {
    error!("[{}] Client loop error: {}", client_name, e);
    return Err(e);
  }

  info!("[{}] Client task finished.", client_name);
  Ok(())
}
