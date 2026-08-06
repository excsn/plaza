//! The lobby: measure the link, form a match, spawn the table it will play at.
//!
//! A controller rather than an HTTP handler because admission needs a latency
//! the server measured, and that only exists on a socket the transport pings.
//!
//! **Where this differs from `lobby_world`.** There, arenas are standing rooms
//! and placement means finding one with a free seat. Here a table is *created*
//! for the match that formed, which is what a card game wants and what a client
//! dialling a per-match endpoint needs. `handle_create_room_request` runs inside
//! `seat_formed`, so the room and the match have the same lifetime.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::controller::ControllerCommand;
use plaza::session::TargetedOp;
use plaza::snapshot::{SnapshotContext, SnapshotError, SnapshotProvider};
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic, StateLogicError};
use plaza_lobby::manager::InMemoryLobbyManager;
use plaza_lobby::op_payloads::{JoinRoomRequestPayload, RoomSettings};
use plaza_lobby::{Formed, LobbyError, MatchQueue, RoomId, TicketRegistry};
use plaza_session::ActixWsPlazaSession;
use tracing::{info, warn};

use crate::bots;
use crate::factory::{TableFactory, TableRegistry};
use crate::types::{
  LinkQuality, LobbyOp, PlayerId, TableCard, TableOp, TableSettings, TABLE_SIZE, TURN_TIMEOUT_TICKS,
};
use crate::wallets::WalletRegistry;

pub type LobbySession = ActixWsPlazaSession<LobbyOp, PlayerId>;

/// Handed out in rotation. A table rather than a random draw, so a demo run is
/// reproducible.
const ASSIGNED_LINKS_MS: [u32; 4] = [0, 25, 70, 140];

/// How long the queue waits for humans before filling the rest with bots.
const PATIENCE: Duration = Duration::from_secs(12);

/// What a seat at a spawned table is worth, and what the losers pay the winner.
const STAKE: u64 = 10;

/// Every table spawned by quick match carries this budget, so the slowest
/// player in a formed match still fits the room it is placed in.
const TABLE_BUDGET_MS: Option<u32> = None;

/// Bot ids start here, well clear of the humans' counter, so a bot is
/// recognisable in a log without consulting anything.
const FIRST_BOT_ID: PlayerId = 1_000_000;

/// Per-player only; shared services live in the logic, already behind an `Arc`.
#[derive(Debug, Clone)]
pub struct LobbyState {
  pub links: HashMap<PlayerId, LinkQuality>,
  /// Outstanding reservations, so they can be cancelled. Ids are per lobby
  /// connection, so one never consumed can never be consumed later.
  pub reserved_in: HashMap<PlayerId, RoomId>,
  pub queue: MatchQueue<PlayerId, Duration>,
  /// Lobby time, the axis the queue's patience is measured on.
  pub now: Duration,
}

impl Default for LobbyState {
  fn default() -> Self {
    Self {
      links: HashMap::new(),
      reserved_in: HashMap::new(),
      queue: MatchQueue::new(TABLE_SIZE, PATIENCE),
      now: Duration::ZERO,
    }
  }
}

pub struct LobbyLogic {
  pub manager: Arc<InMemoryLobbyManager<TableFactory>>,
  pub registry: Arc<TableRegistry>,
  pub wallets: Arc<WalletRegistry>,
  pub tickets: Arc<TicketRegistry<PlayerId>>,
  /// For `agent_rtt`. The controller holds the same `Arc`.
  pub session: Arc<LobbySession>,
  next_link: AtomicUsize,
  next_bot: AtomicU64,
}

impl LobbyLogic {
  pub fn new(
    manager: Arc<InMemoryLobbyManager<TableFactory>>,
    registry: Arc<TableRegistry>,
    wallets: Arc<WalletRegistry>,
    tickets: Arc<TicketRegistry<PlayerId>>,
    session: Arc<LobbySession>,
  ) -> Self {
    Self {
      manager,
      registry,
      wallets,
      tickets,
      session,
      next_link: AtomicUsize::new(0),
      next_bot: AtomicU64::new(FIRST_BOT_ID),
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

  /// One pass, rather than every table reaching into the lobby's metadata.
  ///
  /// Through the registry's own handles: the lobby holds a seam that names no
  /// game types, so refreshing its cached counts is the application's job.
  fn refresh_seat_counts(&self) {
    for handle in self.manager.rooms() {
      let Some(entry) = self.registry.get(&handle.id()) else { continue };
      let Some(room) = self.registry.room(&handle.id()) else { continue };
      room.update_player_count_in_metadata(entry.seats.load(Ordering::Relaxed));
    }
  }

  /// Every live table, marked with whether this link can carry it.
  ///
  /// Usually short or empty, because tables here are spawned per match rather
  /// than standing. What it is for is spectating, and for showing that a match
  /// you were placed in is a real room somebody else can watch.
  fn catalogue(&self, link: LinkQuality) -> Vec<TableCard> {
    self.refresh_seat_counts();

    let playable = self.manager.rooms_playable_at(link.one_way_ms);
    let rank_of: HashMap<RoomId, u32> = playable
      .iter()
      .enumerate()
      .map(|(i, m)| (m.room_id, i as u32))
      .collect();

    let mut cards: Vec<TableCard> = self
      .manager
      .list_rooms(None)
      .into_iter()
      .map(|m| TableCard {
        room_id: m.room_id,
        name: m.name,
        current_players: m.current_players,
        max_players: m.max_players,
        budget_ms: m.max_one_way_ms,
        playable: rank_of.contains_key(&m.room_id),
        fit_rank: rank_of.get(&m.room_id).copied(),
      })
      .collect();

    cards.sort_by_key(|c| c.name.clone());
    cards
  }

  async fn tell_table(&self, room_id: &RoomId, why: &str, op: TableOp) {
    self
      .command_table(room_id, why, ControllerCommand::SubmitSystemOps {
        source_description: why.to_string(),
        ops: vec![op],
      })
      .await;
  }

  async fn command_table(
    &self,
    room_id: &RoomId,
    why: &str,
    command: ControllerCommand<TableOp, PlayerId, crate::types::TableState>,
  ) {
    // Through the factory's registry, not the lobby's handle: the seam names
    // no game types, which is what would let a table live in another process.
    let Some(commands) = self.registry.commands(room_id) else {
      warn!(room = %room_id, why, "Spoke to a table that has since gone.");
      return;
    };
    if commands.send(command).await.is_err() {
      warn!(room = %room_id, why, "Table controller ended before the message landed.");
    }
  }

  /// Seats a match the queue formed, at a table spawned for it.
  ///
  /// Bots join by command rather than by connecting, which is the whole reason
  /// `Agent::Bot` exists: they are participants the transport never sees, so a
  /// broadcast simply never matches them and nothing has to special-case one.
  async fn seat_formed(&self, state: &mut LobbyState, formed: Formed<PlayerId>) -> Vec<TargetedOp<LobbyOp, PlayerId>> {
    let seats = formed.size() as u32;
    let settings = RoomSettings {
      name: Some(format!("table {}", self.registry.len() + 1)),
      game_mode: "tricks".to_string(),
      max_players: seats,
      is_private: false,
      password_hash: None,
      custom_game_settings: TableSettings {
        stake: STAKE,
        turn_timeout_ticks: TURN_TIMEOUT_TICKS,
        budget_ms: TABLE_BUDGET_MS,
      },
    };

    let room = match self.manager.handle_create_room_request(&0, settings).await {
      Ok(metadata) => metadata,
      Err(e) => {
        warn!(error = %e, "Could not open a table for a formed match.");
        return formed
          .players
          .iter()
          .map(|p| {
            TargetedOp::new_system_to(*p, vec![LobbyOp::Refused {
              room_id: RoomId::nil(),
              reason: format!("could not open a table: {e}"),
              measured_one_way_ms: state.links.get(p).map(|l| l.one_way_ms).unwrap_or(0),
              allowed_one_way_ms: None,
            }])
          })
          .collect();
      }
    };

    for _ in 0..formed.bots {
      let bot = self.next_bot.fetch_add(1, Ordering::Relaxed);
      self
        .tell_table(&room.room_id, "quick match bot", TableOp::Reserve { player: bot })
        .await;
      self
        .command_table(&room.room_id, "quick match bot", ControllerCommand::HandleAgentJoined {
          agent: Agent::new_bot(bot),
        })
        .await;
      // Per table, against that table's own channel, and it ends when the
      // controller does.
      if let Some(commands) = self.registry.commands(&room.room_id) {
        tokio::spawn(bots::play(commands, bot));
      }
    }

    let mut out = Vec::new();
    for player in &formed.players {
      self.reserve_seat(state, &room.room_id, *player).await;
      let base = self
        .manager
        .room(&room.room_id)
        .map(|h| h.session_endpoint_info())
        .unwrap_or_default();
      out.push(TargetedOp::new_system_to(*player, vec![LobbyOp::Placed {
        room_id: room.room_id,
        name: room.name.clone(),
        endpoint: self.endpoint_with_ticket(&base, *player, room.room_id),
        spectator: false,
        coins: self.wallets.balance(*player),
      }]));
    }

    info!(
      table = %room.name,
      humans = formed.players.len(),
      bots = formed.bots,
      timed_out = formed.timed_out,
      "Quick match seated."
    );
    out
  }

  /// Cancels any earlier reservation first: holding two would leave the first
  /// table counting a seat nobody is coming to fill.
  async fn reserve_seat(&self, state: &mut LobbyState, room_id: &RoomId, player: PlayerId) {
    if let Some(previous) = state.reserved_in.insert(player, *room_id)
      && previous != *room_id
    {
      self
        .tell_table(&previous, "lobby re-placement", TableOp::Withdraw { player })
        .await;
    }
    self
      .tell_table(room_id, "lobby admission", TableOp::Reserve { player })
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

        Ok(LogicOutput::ops(vec![TargetedOp::new_system_to(player, vec![
          LobbyOp::Welcome {
            you: player,
            link,
            coins: self.wallets.balance(player),
          },
          LobbyOp::Catalogue {
            tables: self.catalogue(link),
            link,
          },
        ])]))
      }

      LogicInput::AgentLeft { agent_id } => {
        state.links.remove(&agent_id);
        state.queue.remove(&agent_id);
        // The departure a table cannot infer: a closing socket means nothing,
        // but leaving the lobby means the seat will never be taken.
        if let Some(room_id) = state.reserved_in.remove(&agent_id) {
          self
            .tell_table(&room_id, "lobby departure", TableOp::Withdraw { player: agent_id })
            .await;
        }
        self.manager.handle_player_leaving_lobby(&agent_id).await;
        self.wallets.forget(agent_id);
        info!(player = agent_id, "Player left the lobby.");
        Ok(LogicOutput::none())
      }

      LogicInput::TimeStep { delta_time } => {
        state.now += delta_time;
        let ready = state.queue.drain_ready(state.now);
        if ready.is_empty() {
          return Ok(LogicOutput::none());
        }
        let mut out = Vec::new();
        for formed in ready {
          out.extend(self.seat_formed(state, formed).await);
        }
        Ok(LogicOutput::ops(out))
      }

      LogicInput::AgentOps { source, ops } => {
        let Some(player) = source.id_cloned() else {
          return Ok(LogicOutput::none());
        };
        let mut out = Vec::new();

        for op in ops {
          match op {
            LobbyOp::ListTables => {
              let extra = state.links.get(&player).map(|l| l.assigned_extra_ms).unwrap_or(0);
              let link = self.link_for(player, extra);
              state.links.insert(player, link);
              out.push(TargetedOp::new_system_to(player, vec![LobbyOp::Catalogue {
                tables: self.catalogue(link),
                link,
              }]));
            }

            LobbyOp::QuickMatch => {
              let extra = state.links.get(&player).map(|l| l.assigned_extra_ms).unwrap_or(0);
              let link = self.link_for(player, extra);
              state.links.insert(player, link);
              state.queue.enqueue(player, state.now);
              out.push(TargetedOp::new_system_to(player, vec![LobbyOp::Queued {
                position: state.queue.position(&player).unwrap_or(0) as u32,
                needed: state.queue.match_size() as u32,
                patience_ms: PATIENCE.as_millis() as u32,
              }]));
            }

            LobbyOp::LeaveQueue => {
              state.queue.remove(&player);
              out.push(TargetedOp::new_system_to(player, vec![LobbyOp::QueueLeft]));
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
                Err(LobbyError::UnsuitableConnection { measured_ms, allowed_ms }) => LobbyOp::Refused {
                  room_id,
                  reason: "This link cannot meet that table's schedule.".into(),
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
            // seat, and a full table is when spectating matters.
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
              return Err(StateLogicError::InvalidOperation(format!("Clients do not send {other:?}.")));
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
