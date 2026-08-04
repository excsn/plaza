use async_trait::async_trait;
use plaza::{
  agent::Agent,
  error::SnapshotError,
  snapshot::{SnapshotContext, SnapshotProvider},
};

use crate::types::{
  FogOp, FogState, PanelView, PlayerId, PlayerView, RelicView, UnitView, RELICS, VISION,
};
use crate::vision::{can_see, visible_relics};

/// Builds one player's world.
///
/// The seam for hidden information, in its spatial form: `card_table` decides
/// what a recipient may hold by asking whose hand it is, and this decides by
/// asking where their eyes are. Same seam, and the query behind it is the only
/// thing that changed.
///
/// **Nothing is filtered downstream of here.** A relic outside your vision is
/// absent from your payload rather than flagged in it, so there is no field a
/// modified client could read to learn what it was not sent.
#[derive(Debug, Default)]
pub struct FogSnapshotter;

/// The view, as a function, so the bots can be given exactly what a browser is
/// given rather than a look at the state.
pub fn player_view(state: &FogState, player: PlayerId) -> PlayerView {
  let (relic_ids, considered) = visible_relics(state, player);

  let relics: Vec<RelicView> = relic_ids
    .iter()
    .map(|id| {
      let relic = &state.relics[*id as usize];
      RelicView {
        id: relic.id,
        x: relic.x,
        y: relic.y,
        owner: relic.owner,
        claimant: relic.claimant,
        progress: relic.progress,
      }
    })
    .collect();

  let my_units: Vec<UnitView> = state
    .units_of(player)
    .map(|u| UnitView {
      id: u.id,
      owner: u.owner,
      x: u.x,
      y: u.y,
    })
    .collect();

  let enemy_units: Vec<UnitView> = state
    .units
    .iter()
    .filter(|u| u.owner != player && can_see(state, player, u.x, u.y))
    .map(|u| UnitView {
      id: u.id,
      owner: u.owner,
      x: u.x,
      y: u.y,
    })
    .collect();

  let mut scores: Vec<(PlayerId, u32)> = state.players.iter().map(|(id, p)| (*id, p.score)).collect();
  scores.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

  let stats = state.players.get(&player).map(|p| p.stats.clone()).unwrap_or_default();
  let withheld_now = state.players.get(&player).map_or(0, |p| p.withheld.len());

  PlayerView {
    you: player,
    tick: state.tick,
    my_units,
    enemy_units,
    relics: relics.clone(),
    scores,
    panel: PanelView {
      relics_in_world: RELICS,
      relics_visible: relics.len(),
      relics_considered: considered,
      told: stats.told,
      told_late: stats.told_late,
      withheld_now,
      leaks: stats.leaks,
      leak_mode: state.leak_mode,
    },
  }
}

#[async_trait]
impl SnapshotProvider<PlayerId, FogState, FogOp> for FogSnapshotter {
  async fn create_snapshot(
    &self,
    state: &FogState,
    target_agent: Option<&Agent<PlayerId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<FogOp>, SnapshotError<PlayerId>> {
    // No recipient, no view. A uniform pass over this provider would be a
    // world nobody is allowed to hold, so there is nothing sensible to return
    // and returning the whole map "just for the join case" is how a leak gets
    // written.
    let Some(player) = target_agent.and_then(|a| a.id_cloned()) else {
      return Ok(None);
    };
    if !state.players.contains_key(&player) {
      return Ok(None);
    }
    Ok(Some(FogOp::Snapshot(Box::new(player_view(state, player)))))
  }
}

/// Vision radius, re-exported for the page's fog rendering.
pub const VISION_RADIUS: f32 = VISION;
