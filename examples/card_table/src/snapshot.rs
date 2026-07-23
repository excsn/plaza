use crate::types::{PlayerId, PlayerView, TableState};
use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::error::SnapshotError;
use plaza::game_common::flow_control::{RoundManager, TurnManager};
use plaza::game_common::scorekeeping::Scorekeeper;
use plaza::snapshot::{SnapshotContext, SnapshotData, SnapshotProvider};

/// Builds one player's view of the table.
///
/// This is the seam for hidden information. The controller calls this once per
/// recipient and hands over `target_agent`, so the same state yields a different
/// payload for each player: your own cards by rank, everyone else's by count.
///
/// Getting this wrong leaks the game. Note that nothing else in the example can
/// leak a hand, because this is the only place a hand is turned into something
/// a client receives.
#[derive(Debug, Default)]
pub struct TableSnapshotter;

#[async_trait]
impl SnapshotProvider<PlayerId, TableState, PlayerView> for TableSnapshotter {
  async fn create_snapshot_data(
    &self,
    state: &TableState,
    target_agent: Option<&Agent<PlayerId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<SnapshotData<PlayerView>, SnapshotError<PlayerId>> {
    let me = target_agent.and_then(|a| a.id_cloned());

    let my_hand = me
      .as_ref()
      .and_then(|id| state.hands.get(id))
      .cloned()
      .unwrap_or_default();

    // Everyone who is not the recipient, reduced to a card count.
    let opponents = state
      .seats
      .iter()
      .filter(|id| Some(**id) != me)
      .map(|id| (*id, state.hands.get(id).map_or(0, Vec::len)))
      .collect();

    Ok(SnapshotData {
      payload: PlayerView {
        phase: *state.phase.current(),
        round: state.rounds.current_round(),
        total_rounds: state.rounds.max_rounds(),
        whose_turn: state.turns.current_turn_actor(),
        my_hand,
        opponents,
        table: state.table.clone(),
        scores: state.scores.get_all_scores_sorted(),
      },
    })
  }
}
