//! The lobby: measure the link, show what fits, place the player.
//!
//! A controller rather than an HTTP handler because admission needs a latency
//! the server measured, and that only exists on a socket the transport pings.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::controller::ControllerCommand;
use plaza::session::TargetedOp;
use plaza::snapshot::{SnapshotContext, SnapshotError, SnapshotProvider};
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic, StateLogicError};
use plaza_lobby::manager::InMemoryLobbyManager;
use plaza_lobby::op_payloads::JoinRoomRequestPayload;
use plaza_lobby::{LobbyError, RoomHandle, RoomId};
use plaza_session::ActixWsPlazaSession;
use tracing::{info, warn};

use crate::factory::{ArenaFactory, RoomRegistry};
use crate::tickets::TicketRegistry;
use crate::types::{LinkQuality, LobbyOp, PlayerId, RoomCard};
use crate::wallets::WalletRegistry;

pub type LobbySession = ActixWsPlazaSession<LobbyOp, PlayerId>;

/// Handed out in rotation. A table rather than a random draw, so a demo run is
/// reproducible.
const ASSIGNED_LINKS_MS: [u32; 4] = [0, 25, 70, 140];

/// Per-player only; shared services live in the logic, already behind an `Arc`.
#[derive(Debug, Clone, Default)]
pub struct LobbyState {
  pub links: HashMap<PlayerId, LinkQuality>,
  /// Outstanding reservations, so they can be cancelled. Ids are per lobby
  /// connection, so one never consumed can never be consumed later.
  pub reserved_in: HashMap<PlayerId, RoomId>,
}

pub struct LobbyLogic {
  pub manager: Arc<InMemoryLobbyManager<ArenaFactory>>,
  pub registry: Arc<RoomRegistry>,
  pub wallets: Arc<WalletRegistry>,
  pub tickets: Arc<TicketRegistry>,
  /// For `agent_rtt`. The controller holds the same `Arc`.
  pub session: Arc<LobbySession>,
  next_link: AtomicUsize,
}

impl LobbyLogic {
  pub fn new(
    manager: Arc<InMemoryLobbyManager<ArenaFactory>>,
    registry: Arc<RoomRegistry>,
    wallets: Arc<WalletRegistry>,
    tickets: Arc<TicketRegistry>,
    session: Arc<LobbySession>,
  ) -> Self {
    Self {
      manager,
      registry,
      wallets,
      tickets,
      session,
      next_link: AtomicUsize::new(0),
    }
  }

  fn endpoint_with_ticket(&self, base: &str, player: PlayerId, room: RoomId) -> String {
    format!("{base}?t={}", self.tickets.issue(player, room))
  }

  fn assign_extra_ms(&self) -> u32 {
    let n = self.next_link.fetch_add(1, Ordering::Relaxed);
    ASSIGNED_LINKS_MS[n % ASSIGNED_LINKS_MS.len()]
  }

  /// Absent for a connection barely a moment old: the transport pings eight
  /// times at 125ms before settling. Reads as zero, so the client re-lists.
  fn link_for(&self, player: PlayerId, extra_ms: u32) -> LinkQuality {
    let measured = self
      .session
      .agent_rtt(&player)
      .map(|(rtt, _samples)| rtt.as_millis() as u32)
      .unwrap_or(0);
    LinkQuality::new(measured, extra_ms)
  }

  /// One pass, rather than every arena reaching into the lobby's metadata.
  fn refresh_seat_counts(&self) {
    for handle in self.manager.rooms() {
      if let Some(entry) = self.registry.get(&handle.id()) {
        handle.update_player_count_in_metadata(entry.seats.load(Ordering::Relaxed));
      }
    }
  }

  /// Every room, marked with whether this link can carry it. Showing only the
  /// playable subset would conflate "none for you" with "none at all".
  fn catalogue(&self, link: LinkQuality) -> Vec<RoomCard> {
    self.refresh_seat_counts();

    let playable = self.manager.rooms_playable_at(link.one_way_ms);
    let rank_of: HashMap<RoomId, u32> = playable
      .iter()
      .enumerate()
      .map(|(i, m)| (m.room_id, i as u32))
      .collect();

    let mut cards: Vec<RoomCard> = self
      .manager
      .list_rooms(None)
      .into_iter()
      .map(|m| RoomCard {
        room_id: m.room_id,
        name: m.name,
        current_players: m.current_players,
        max_players: m.max_players,
        budget_ms: m.max_one_way_ms,
        playable: rank_of.contains_key(&m.room_id),
        fit_rank: rank_of.get(&m.room_id).copied(),
      })
      .collect();

    // Tightest first, matching `routing::playable_at`.
    cards.sort_by_key(|c| (c.budget_ms.unwrap_or(u32::MAX), c.name.clone()));
    cards
  }

  async fn tell_arena(&self, room_id: &RoomId, why: &str, op: crate::types::RoomOp) {
    let Some(handle) = self.manager.room(room_id) else {
      warn!(room = %room_id, why, "Spoke to a room that has since gone.");
      return;
    };
    let command = ControllerCommand::SubmitSystemOps {
      source_description: why.to_string(),
      ops: vec![op],
    };
    if handle.command_tx.send(command).await.is_err() {
      warn!(room = %room_id, why, "Arena controller ended before the message landed.");
    }
  }

  /// Cancels any earlier reservation first: holding two would leave the first
  /// arena counting a seat nobody is coming to fill.
  async fn reserve_seat(&self, state: &mut LobbyState, room_id: &RoomId, player: PlayerId) {
    if let Some(previous) = state.reserved_in.insert(player, *room_id)
      && previous != *room_id
    {
      self
        .tell_arena(&previous, "lobby re-placement", crate::types::RoomOp::Withdraw { player })
        .await;
    }
    self
      .tell_arena(room_id, "lobby admission", crate::types::RoomOp::Reserve { player })
      .await;
  }
}

#[async_trait]
impl StateLogic<LobbyOp, PlayerId, LobbyState> for LobbyLogic {
  async fn process_input(
    &self,
    state: &mut LobbyState,
    input: LogicInput<LobbyOp, PlayerId>,
  ) -> Result<LogicOutput<LobbyOp, PlayerId>, StateLogicError> {
    match input {
      LogicInput::AgentJoined { agent } => {
        let Some(player) = agent.id_cloned() else {
          return Ok(LogicOutput::none());
        };
        let link = self.link_for(player, self.assign_extra_ms());
        state.links.insert(player, link);
        info!(player, one_way_ms = link.one_way_ms, "Player entered the lobby.");

        Ok(LogicOutput::ops(vec![TargetedOp::new_system_to(
          player,
          vec![
            LobbyOp::Welcome {
              you: player,
              link,
              coins: self.wallets.balance(player),
            },
            LobbyOp::Catalogue {
              rooms: self.catalogue(link),
              link,
            },
          ],
        )]))
      }

      LogicInput::AgentLeft { agent_id } => {
        state.links.remove(&agent_id);
        // The departure an arena cannot infer: a closing socket means nothing,
        // but leaving the lobby means the seat will never be taken.
        if let Some(room_id) = state.reserved_in.remove(&agent_id) {
          self
            .tell_arena(&room_id, "lobby departure", crate::types::RoomOp::Withdraw {
              player: agent_id.clone(),
            })
            .await;
        }
        self.manager.handle_player_leaving_lobby(&agent_id).await;
        // The world, not a room, so the wallet goes too.
        self.wallets.forget(agent_id);
        info!(player = agent_id, "Player left the lobby.");
        Ok(LogicOutput::none())
      }

      LogicInput::TimeStep { .. } => Ok(LogicOutput::none()),

      LogicInput::AgentOps { source, ops } => {
        let Some(player) = source.id_cloned() else {
          return Ok(LogicOutput::none());
        };
        let mut out = Vec::new();

        for op in ops {
          match op {
            LobbyOp::ListRooms => {
              let extra = state.links.get(&player).map(|l| l.assigned_extra_ms).unwrap_or(0);
              let link = self.link_for(player, extra);
              state.links.insert(player, link);
              out.push(TargetedOp::new_system_to(
                player,
                vec![LobbyOp::Catalogue {
                  rooms: self.catalogue(link),
                  link,
                }],
              ));
            }

            LobbyOp::Reroll => {
              let link = self.link_for(player, self.assign_extra_ms());
              state.links.insert(player, link);
              out.push(TargetedOp::new_system_to(
                player,
                vec![LobbyOp::Catalogue {
                  rooms: self.catalogue(link),
                  link,
                }],
              ));
            }

            LobbyOp::Join { room_id } => {
              self.refresh_seat_counts();
              let extra = state.links.get(&player).map(|l| l.assigned_extra_ms).unwrap_or(0);
              let link = self.link_for(player, extra);
              state.links.insert(player, link);

              let payload = JoinRoomRequestPayload {
                measured_one_way_ms: Some(link.one_way_ms),
                room_id,
                password_attempt: None,
              };

              let reply = match self
                .manager
                .handle_join_room_request(&player, Agent::new_human(player), &payload)
                .await
              {
                Ok(outcome) => {
                  self.reserve_seat(state, &room_id, player).await;
                  let name = self
                    .manager
                    .room(&room_id)
                    .map(|h| h.metadata().name)
                    .unwrap_or_default();
                  let base = outcome.room_session_endpoint.unwrap_or_default();
                  LobbyOp::Placed {
                    room_id,
                    name,
                    endpoint: self.endpoint_with_ticket(&base, player, room_id),
                    spectator: false,
                    coins: self.wallets.balance(player),
                  }
                }
                // Its own variant, not a string, so the client can compare the
                // two numbers and offer a room that fits.
                Err(LobbyError::UnsuitableConnection { measured_ms, allowed_ms }) => LobbyOp::Refused {
                  room_id,
                  reason: "This link cannot meet that arena's schedule.".into(),
                  measured_one_way_ms: measured_ms,
                  allowed_one_way_ms: Some(allowed_ms),
                },
                Err(other) => LobbyOp::Refused {
                  room_id,
                  reason: other.to_string(),
                  measured_one_way_ms: link.one_way_ms,
                  allowed_one_way_ms: None,
                },
              };

              out.push(TargetedOp::new_system_to(player, vec![reply]));
            }

            // Deliberately not `handle_join_room_request`: a spectator takes no
            // seat, and a full arena is when spectating matters.
            LobbyOp::Spectate { room_id } => {
              let reply = match self.manager.room(&room_id) {
                Some(handle) => LobbyOp::Placed {
                  room_id,
                  name: handle.metadata().name,
                  endpoint: self.endpoint_with_ticket(&handle.session_endpoint_info(), player, room_id),
                  spectator: true,
                  coins: self.wallets.balance(player),
                },
                None => LobbyOp::Refused {
                  room_id,
                  reason: LobbyError::RoomNotFound(room_id).to_string(),
                  measured_one_way_ms: state.links.get(&player).map(|l| l.one_way_ms).unwrap_or(0),
                  allowed_one_way_ms: None,
                },
              };
              out.push(TargetedOp::new_system_to(player, vec![reply]));
            }

            other => {
              return Err(StateLogicError::InvalidOperation(format!(
                "Clients do not send {other:?}."
              )));
            }
          }
        }

        Ok(LogicOutput::ops(out))
      }
    }
  }
}

/// Everything the lobby says is a reply to a request, so there is no snapshot.
pub struct NoLobbySnapshot;

#[async_trait]
impl SnapshotProvider<PlayerId, LobbyState, LobbyOp> for NoLobbySnapshot {
  async fn create_snapshot(
    &self,
    _state: &LobbyState,
    _target: Option<&Agent<PlayerId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<LobbyOp>, SnapshotError<PlayerId>> {
    Ok(None)
  }
}
