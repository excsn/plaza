//! Arbitration: collect every claim for an item, then decide once.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::common::participants::ParticipantTracker;
use plaza::session::TargetedOp;
use plaza::snapshot::{SnapshotContext, SnapshotError, SnapshotProvider};
use plaza::state_logic::{LogicInput, LogicOutput, SnapshotRequest, StateLogic, StateLogicError};
use plaza_session::ActixWsPlazaSession;

use crate::types::{
  AuctionOp, Contender, FloorView, Item, ItemId, PlayerId, Rejection, Standing, Tick, TICK_HZ, WINDOW,
};

pub type FloorSession = ActixWsPlazaSession<AuctionOp, PlayerId>;

/// Ticks between drops.
const DROP_EVERY: Tick = 30;

/// Values cycle rather than being drawn at random, so two runs of the readout
/// are comparable and nothing here needs a generator.
const VALUES: [u64; 4] = [10, 25, 50, 100];

#[derive(Debug, Clone)]
struct Claim {
  player: PlayerId,
  req: u64,
  named: Tick,
}

#[derive(Debug, Clone, Default)]
pub struct Floor {
  pub tick: Tick,
  pub items: HashMap<ItemId, Item>,
  /// Claims per item, in arrival order. Order is kept only for the readout;
  /// nothing about the outcome depends on it, which is the property the whole
  /// example exists to show.
  claims: HashMap<ItemId, Vec<Claim>>,
  pub players: ParticipantTracker<PlayerId, Standing>,
  next_item: ItemId,
  next_drop: Tick,
}

impl Floor {
  pub fn new() -> Self {
    Self {
      next_drop: DROP_EVERY,
      ..Default::default()
    }
  }

  fn closes_at(&self, item: &Item) -> Tick {
    item.dropped_at + WINDOW
  }

  pub fn view_for(&self, viewer: Option<&PlayerId>, floor_ticks: Tick, rtt_ms: u32) -> FloorView {
    let mut items: Vec<Item> = self.items.values().cloned().collect();
    items.sort_by_key(|i| i.id);
    let mut standings: Vec<(PlayerId, Standing)> = self
      .players
      .iter()
      .map(|(id, info)| (*id, info.app_data.clone()))
      .collect();
    standings.sort_by_key(|(id, _)| *id);

    FloorView {
      tick: self.tick,
      window: WINDOW,
      items,
      standings,
      your_floor: floor_ticks,
      your_rtt_ms: rtt_ms,
      you: viewer.copied().unwrap_or(0),
    }
  }
}

pub struct AuctionLogic {
  /// Held for `agent_rtt`. The bound on a legal claim is a number this measured,
  /// never one a client reported.
  session: Arc<FloorSession>,
}

impl AuctionLogic {
  pub fn new(session: Arc<FloorSession>) -> Self {
    Self { session }
  }

  fn rtt_ms(&self, player: PlayerId) -> u32 {
    self
      .session
      .agent_rtt(&player)
      .map(|(rtt, _)| rtt.as_millis() as u32)
      .unwrap_or(0)
  }

  /// The earliest tick this connection could legally name for a drop.
  ///
  /// One way, rounded down, in ticks. Rounded *down* deliberately: rounding up
  /// would refuse honest claims from anyone whose latency sits just over a tick
  /// boundary, and refusing a real player to catch a hypothetical one is the
  /// wrong trade.
  fn claim_floor(&self, player: PlayerId) -> Tick {
    let one_way_ms = self.rtt_ms(player) / 2;
    (one_way_ms as u64 * TICK_HZ as u64) / 1000
  }

  /// Ranks the claims for one item.
  ///
  /// Lowest named tick wins. Ties break on a hash of the player and the item,
  /// which is arbitrary but fixed: the alternative is arrival order, and arrival
  /// order is ping, which is the thing this is built to make irrelevant.
  fn rank(item: ItemId, claims: &[Claim]) -> Vec<Claim> {
    let mut ranked = claims.to_vec();
    ranked.sort_by_key(|c| {
      let mix = c.player.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (item as u64).wrapping_mul(0xC2B2_AE3D);
      (c.named, mix)
    });
    ranked
  }
}

#[async_trait]
impl StateLogic<AuctionOp, PlayerId, Floor> for AuctionLogic {
  async fn process_input(
    &self,
    state: &mut Floor,
    input: LogicInput<AuctionOp, PlayerId>,
  ) -> Result<LogicOutput<AuctionOp, PlayerId>, StateLogicError> {
    match input {
      LogicInput::AgentJoined { agent } => {
        let Some(id) = agent.id_cloned() else {
          return Ok(LogicOutput::none());
        };
        state.players.add_participant(agent, Standing::default());
        Ok(LogicOutput::ops(vec![TargetedOp::new_system_to(
          id,
          vec![AuctionOp::Welcome {
            you: id,
            hz: TICK_HZ,
            window: WINDOW,
          }],
        )]))
      }

      LogicInput::AgentLeft { agent_id } => {
        state.players.remove_participant(&agent_id);
        for claims in state.claims.values_mut() {
          claims.retain(|c| c.player != agent_id);
        }
        Ok(LogicOutput::none())
      }

      LogicInput::TimeStep { .. } => {
        state.tick += 1;
        let now = state.tick;
        let mut out = Vec::new();

        if now >= state.next_drop {
          state.next_drop = now + DROP_EVERY;
          let id = state.next_item;
          state.next_item += 1;
          let item = Item {
            id,
            value: VALUES[(id as usize) % VALUES.len()],
            dropped_at: now,
            lane: (id % 4) as u8,
          };
          state.items.insert(id, item.clone());
          out.push(TargetedOp::new_system_all(vec![AuctionOp::Dropped { item }]));
        }

        // Contests that closed on this tick. Collected first so the map is not
        // borrowed while the awards are built.
        let closing: Vec<ItemId> = state
          .items
          .values()
          .filter(|item| state.closes_at(item) <= now)
          .map(|item| item.id)
          .collect();

        for id in closing {
          let Some(item) = state.items.remove(&id) else { continue };
          let claims = state.claims.remove(&id).unwrap_or_default();
          if claims.is_empty() {
            out.push(TargetedOp::new_system_all(vec![AuctionOp::Expired { item: id }]));
            continue;
          }

          let ranked = Self::rank(id, &claims);
          let contenders: Vec<Contender> = ranked
            .iter()
            .map(|c| Contender {
              player: c.player,
              named: c.named,
            })
            .collect();
          let winner = ranked[0].clone();
          let margin = ranked.get(1).map(|r| r.named - winner.named).unwrap_or(0);

          if let Some(standing) = state.players.get_participant_app_data_mut(&winner.player) {
            standing.won += 1;
            standing.score += item.value;
          }
          out.push(TargetedOp::new_system_to(winner.player, vec![AuctionOp::Awarded {
            req: winner.req,
            item: id,
            value: item.value,
            named: winner.named,
            margin,
            contenders: contenders.clone(),
          }]));

          for beaten in ranked.iter().skip(1) {
            if let Some(standing) = state.players.get_participant_app_data_mut(&beaten.player) {
              standing.lost += 1;
            }
            out.push(TargetedOp::new_system_to(beaten.player, vec![AuctionOp::Lost {
              req: beaten.req,
              item: id,
              to: winner.player,
              named: beaten.named,
              winner_named: winner.named,
              contenders: contenders.clone(),
            }]));
          }

          // The public record. No `req`, because it is nobody's reply: it is
          // what everyone sees regardless of whether they took part.
          out.push(TargetedOp::new_system_all(vec![AuctionOp::Taken {
            item: id,
            by: winner.player,
            value: item.value,
          }]));
        }

        let everyone: Vec<Agent<PlayerId>> = state.players.iter().map(|(_, i)| i.agent.clone()).collect();
        Ok(LogicOutput::ops(out).and_snapshot(SnapshotRequest::to(everyone)))
      }

      LogicInput::AgentOps { source, ops } => {
        let Some(player) = source.id_cloned() else {
          return Ok(LogicOutput::none());
        };
        let mut out = Vec::new();

        for op in ops {
          let AuctionOp::Grab { req, item, tick } = op else {
            return Err(StateLogicError::InvalidOperation("Clients only send Grab.".into()));
          };

          let refuse = |why: Rejection| {
            TargetedOp::new_system_to(player, vec![AuctionOp::Refused { req, item, why }])
          };

          let Some(on_floor) = state.items.get(&item).cloned() else {
            out.push(refuse(Rejection::NoSuchItem));
            continue;
          };

          let floor = self.claim_floor(player);
          let earliest = on_floor.dropped_at + floor;
          let closes = on_floor.dropped_at + WINDOW;

          if tick < earliest {
            out.push(refuse(Rejection::TooEarly {
              floor: earliest,
              named: tick,
            }));
            continue;
          }
          if tick > closes {
            out.push(refuse(Rejection::TooLate { closed: closes, named: tick }));
            continue;
          }

          let entry = state.claims.entry(item).or_default();
          if entry.iter().any(|c| c.player == player) {
            out.push(refuse(Rejection::Duplicate));
            continue;
          }
          entry.push(Claim {
            player,
            req,
            named: tick,
          });
        }

        if !out.is_empty()
          && let Some(standing) = state.players.get_participant_app_data_mut(&player)
        {
          standing.rejected += out.len() as u32;
        }
        Ok(LogicOutput::ops(out))
      }
    }
  }
}

pub struct FloorSnapshotter {
  session: Arc<FloorSession>,
}

impl FloorSnapshotter {
  pub fn new(session: Arc<FloorSession>) -> Self {
    Self { session }
  }
}

#[async_trait]
impl SnapshotProvider<PlayerId, Floor, AuctionOp> for FloorSnapshotter {
  async fn create_snapshot(
    &self,
    state: &Floor,
    target: Option<&Agent<PlayerId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<AuctionOp>, SnapshotError<PlayerId>> {
    let viewer = target.and_then(|a| a.id());
    let (floor_ticks, rtt) = match viewer {
      Some(id) => {
        let rtt = self.session.agent_rtt(id).map(|(r, _)| r.as_millis() as u32).unwrap_or(0);
        (((rtt / 2) as u64 * TICK_HZ as u64) / 1000, rtt)
      }
      None => (0, 0),
    };
    Ok(Some(AuctionOp::Frame(Box::new(
      state.view_for(viewer, floor_ticks, rtt),
    ))))
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::time::Duration;

  fn logic() -> AuctionLogic {
    AuctionLogic::new(ActixWsPlazaSession::new())
  }

  async fn join(l: &AuctionLogic, state: &mut Floor, id: PlayerId) {
    l.process_input(state, LogicInput::AgentJoined {
      agent: Agent::new_human(id),
    })
    .await
    .unwrap();
  }

  async fn tick(l: &AuctionLogic, state: &mut Floor) -> LogicOutput<AuctionOp, PlayerId> {
    l.process_input(state, LogicInput::TimeStep {
      delta_time: Duration::from_millis(50),
    })
    .await
    .unwrap()
  }

  async fn grab(l: &AuctionLogic, state: &mut Floor, who: PlayerId, req: u64, item: ItemId, at: Tick) -> LogicOutput<AuctionOp, PlayerId> {
    l.process_input(state, LogicInput::AgentOps {
      source: Agent::new_human(who),
      ops: vec![AuctionOp::Grab { req, item, tick: at }],
    })
    .await
    .unwrap()
  }

  fn ops_of(out: &LogicOutput<AuctionOp, PlayerId>) -> Vec<&AuctionOp> {
    out.ops.iter().flat_map(|t| t.ops.iter()).collect()
  }

  /// Runs to the first drop and returns it.
  async fn first_item(l: &AuctionLogic, state: &mut Floor) -> Item {
    loop {
      let out = tick(l, state).await;
      if let Some(AuctionOp::Dropped { item }) = ops_of(&out).into_iter().find(|o| matches!(o, AuctionOp::Dropped { .. })) {
        return item.clone();
      }
    }
  }

  #[tokio::test]
  async fn an_uncontested_claim_is_awarded_when_the_window_closes() {
    let l = logic();
    let mut state = Floor::new();
    join(&l, &mut state, 1).await;
    let item = first_item(&l, &mut state).await;

    grab(&l, &mut state, 1, 7, item.id, item.dropped_at + 3).await;
    let mut awarded = None;
    for _ in 0..(WINDOW + 2) {
      let out = tick(&l, &mut state).await;
      for op in ops_of(&out) {
        if let AuctionOp::Awarded { req, margin, .. } = op {
          awarded = Some((*req, *margin));
        }
      }
    }
    assert_eq!(awarded, Some((7, 0)), "the request id comes back with the award");
    assert_eq!(state.players.get_participant_app_data(&1).unwrap().score, item.value);
  }

  /// The property the example exists for: the earlier *named* tick wins, and
  /// nothing about who asked first enters into it.
  #[tokio::test]
  async fn the_earlier_named_tick_wins_regardless_of_arrival_order() {
    let l = logic();
    let mut state = Floor::new();
    join(&l, &mut state, 1).await;
    join(&l, &mut state, 2).await;
    let item = first_item(&l, &mut state).await;

    // Player 2 asks first, but names a later tick.
    grab(&l, &mut state, 2, 20, item.id, item.dropped_at + 5).await;
    grab(&l, &mut state, 1, 10, item.id, item.dropped_at + 2).await;

    let mut winner = None;
    let mut loser_req = None;
    for _ in 0..(WINDOW + 2) {
      for op in ops_of(&tick(&l, &mut state).await) {
        match op {
          AuctionOp::Awarded { req, margin, .. } => winner = Some((*req, *margin)),
          AuctionOp::Lost { req, to, .. } => loser_req = Some((*req, *to)),
          _ => {}
        }
      }
    }
    assert_eq!(winner, Some((10, 3)), "player 1 won by three ticks");
    assert_eq!(loser_req, Some((20, 1)), "player 2 is told, by their own request id");
  }

  /// Every reply is correlated, so several claims can be outstanding at once
  /// and each is answered on its own terms. This is what "your last one was
  /// refused" cannot do.
  #[tokio::test]
  async fn concurrent_claims_are_answered_separately() {
    let l = logic();
    let mut state = Floor::new();
    join(&l, &mut state, 1).await;
    let first = first_item(&l, &mut state).await;
    let out = grab(&l, &mut state, 1, 100, 9999, first.dropped_at + 1).await;
    let refused: Vec<u64> = ops_of(&out)
      .into_iter()
      .filter_map(|o| match o {
        AuctionOp::Refused { req, .. } => Some(*req),
        _ => None,
      })
      .collect();
    assert_eq!(refused, vec![100], "the refusal names the request, not the player");
  }

  #[tokio::test]
  async fn a_claim_after_the_window_is_refused_with_both_numbers() {
    let l = logic();
    let mut state = Floor::new();
    join(&l, &mut state, 1).await;
    let item = first_item(&l, &mut state).await;

    let out = grab(&l, &mut state, 1, 5, item.id, item.dropped_at + WINDOW + 1).await;
    match ops_of(&out)[0] {
      AuctionOp::Refused {
        why: Rejection::TooLate { closed, named },
        ..
      } => {
        assert_eq!(*closed, item.dropped_at + WINDOW);
        assert_eq!(*named, item.dropped_at + WINDOW + 1);
      }
      other => panic!("expected TooLate, got {other:?}"),
    }
  }

  #[tokio::test]
  async fn one_player_cannot_stack_claims_on_one_item() {
    let l = logic();
    let mut state = Floor::new();
    join(&l, &mut state, 1).await;
    let item = first_item(&l, &mut state).await;

    grab(&l, &mut state, 1, 1, item.id, item.dropped_at + 4).await;
    let out = grab(&l, &mut state, 1, 2, item.id, item.dropped_at + 1).await;
    assert!(matches!(
      ops_of(&out)[0],
      AuctionOp::Refused {
        why: Rejection::Duplicate,
        ..
      }
    ));
  }

  #[tokio::test]
  async fn an_unclaimed_item_expires_rather_than_lingering() {
    let l = logic();
    let mut state = Floor::new();
    join(&l, &mut state, 1).await;
    let item = first_item(&l, &mut state).await;
    let mut expired = false;
    for _ in 0..(WINDOW + 2) {
      for op in ops_of(&tick(&l, &mut state).await) {
        if let AuctionOp::Expired { item: id } = op {
          expired = *id == item.id;
        }
      }
    }
    assert!(expired);
    assert!(state.items.is_empty());
  }

  /// Ties are broken the same way every time, so a run is reproducible and the
  /// winner does not depend on map iteration order.
  #[tokio::test]
  async fn ties_break_deterministically() {
    let claims = vec![
      Claim { player: 1, req: 1, named: 5 },
      Claim { player: 2, req: 2, named: 5 },
      Claim { player: 3, req: 3, named: 5 },
    ];
    let first = AuctionLogic::rank(42, &claims);
    let shuffled = vec![claims[2].clone(), claims[0].clone(), claims[1].clone()];
    let second = AuctionLogic::rank(42, &shuffled);
    assert_eq!(
      first.iter().map(|c| c.player).collect::<Vec<_>>(),
      second.iter().map(|c| c.player).collect::<Vec<_>>()
    );
  }

  #[tokio::test]
  async fn a_departing_player_stops_contending() {
    let l = logic();
    let mut state = Floor::new();
    join(&l, &mut state, 1).await;
    join(&l, &mut state, 2).await;
    let item = first_item(&l, &mut state).await;
    grab(&l, &mut state, 1, 1, item.id, item.dropped_at + 1).await;
    grab(&l, &mut state, 2, 2, item.id, item.dropped_at + 4).await;

    l.process_input(&mut state, LogicInput::AgentLeft { agent_id: 1 })
      .await
      .unwrap();

    let mut winner = None;
    for _ in 0..(WINDOW + 2) {
      for op in ops_of(&tick(&l, &mut state).await) {
        if let AuctionOp::Awarded { req, .. } = op {
          winner = Some(*req);
        }
      }
    }
    assert_eq!(winner, Some(2), "the remaining player takes it");
  }
}
