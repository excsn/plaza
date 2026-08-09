use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::error::SnapshotError;
use plaza::game_common::flow_control::{RoundManager, TurnManager};
use plaza::game_common::scorekeeping::Scorekeeper;
use plaza::snapshot::{SnapshotContext, SnapshotProvider};

use crate::types::{PlayerId, PlayerView, TableOp, TableState};

/// Builds one player's view of the table.
///
/// This is the seam for hidden information. The controller calls it once per
/// recipient and hands over `target_agent`, so the same state yields a different
/// payload for each player: your own cards by rank, everyone else's by count.
///
/// Nothing else in the example can leak a hand, because this is the only place
/// a hand becomes something a client receives.
#[derive(Debug, Default)]
pub struct TableSnapshotter;

/// One player's view, as a function.
///
/// Public so a bot can be handed exactly what a browser is handed. A bot that
/// read `TableState` would see every hand at the table, which is the one thing
/// this example exists to say cannot happen.
///
/// `me` is `None` for a spectator and for the uniform pass, and both want the
/// same answer: no hand at all.
pub fn player_view(state: &TableState, me: Option<PlayerId>) -> PlayerView {
  let my_hand = me
    .as_ref()
    .and_then(|id| state.hands.get(id))
    .cloned()
    .unwrap_or_default();

  let opponents = state
    .players()
    .into_iter()
    .filter(|id| Some(*id) != me)
    .map(|id| (id, state.hands.get(&id).map_or(0, Vec::len)))
    .collect();

  PlayerView {
    table: state.name.clone(),
    phase: *state.phase.current(),
    round: state.rounds.current_round(),
    total_rounds: state.rounds.max_rounds(),
    whose_turn: state.turns.current_turn_actor(),
    your_seat: me.and_then(|id| state.seat_of(&id)),
    stake: state.settings.stake,
    coins: me.map(|id| state.wallets.balance(id)).unwrap_or(0),
    my_hand,
    opponents,
    played: state.played.clone(),
    scores: state.scores.get_all_scores_sorted(),
    seats_taken: state.seated_players(),
    seats_total: state.max_players,
    spectators: state.spectators(),
    bots: state.bots(),
  }
}

#[async_trait]
impl SnapshotProvider<PlayerId, TableState, TableOp> for TableSnapshotter {
  async fn create_snapshot(
    &self,
    state: &TableState,
    target_agent: Option<&Agent<PlayerId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<TableOp>, SnapshotError<PlayerId>> {
    let me = target_agent.and_then(|a| a.id_cloned());
    Ok(Some(TableOp::Snapshot(Box::new(player_view(state, me)))))
  }
}
