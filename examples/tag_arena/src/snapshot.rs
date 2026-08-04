use crate::types::{ArenaOp, ArenaState, PlayerId, RunnerView, WorldSnapshot, IDLE_TICKS};
use async_trait::async_trait;
use plaza::{
  agent::Agent,
  snapshot::{SnapshotContext, SnapshotError, SnapshotProvider},
};


/// The world as anyone sees it.
///
/// Public because the server-side bots read the same view a browser does,
/// through `query_with` rather than a socket: a bot that plays from privileged
/// state is not playing the same game.
pub fn world_view(state: &ArenaState) -> WorldSnapshot {
  let mut runners: Vec<RunnerView> = state
    .runners
    .iter()
    .map(|(id, r)| RunnerView {
      id: *id,
      x: r.x,
      y: r.y,
      tags: r.tags,
      bot: r.bot,
      in_play: r.idle_ticks < IDLE_TICKS,
    })
    .collect();
  runners.sort_by_key(|r| r.id);

  let no_tag_back = (state.tick < state.no_tag_back_until)
    .then_some(state.prev_it)
    .flatten();
  WorldSnapshot {
    tick: state.tick,
    it: state.it,
    no_tag_back,
    runners,
  }
}

/// One view for everyone. The uniform tick pass calls this with
/// `target_agent: None` and the join pass with `Some`, and both get the same
/// expression because the world holds nothing recipient-specific: there is no
/// hidden information in a game of tag.
#[derive(Debug, Default)]
pub struct WorldSnapshotProvider;

#[async_trait]
impl SnapshotProvider<PlayerId, ArenaState, ArenaOp> for WorldSnapshotProvider {
  async fn create_snapshot(
    &self,
    state: &ArenaState,
    _target_agent: Option<&Agent<PlayerId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<ArenaOp>, SnapshotError<PlayerId>> {
    Ok(Some(ArenaOp::Snapshot(Box::new(world_view(state)))))
  }
}
