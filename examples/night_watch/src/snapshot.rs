//! The view, built once per recipient. `your_role` differs for every player
//! and `everyone` goes to the dead but not the living, so a uniform snapshot
//! could not work for this game.

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::error::SnapshotError;
use plaza::snapshot::{SnapshotContext, SnapshotProvider};

use crate::types::{PlayerId, VillageOp, VillageState, VillageView};

#[derive(Debug, Default)]
pub struct VillageSnapshotter;

#[async_trait]
impl SnapshotProvider<PlayerId, VillageState, VillageOp> for VillageSnapshotter {
  async fn create_snapshot(
    &self,
    state: &VillageState,
    target_agent: Option<&Agent<PlayerId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<VillageOp>, SnapshotError<PlayerId>> {
    let me = target_agent.and_then(|a| a.id_cloned());
    Ok(Some(VillageOp::Snapshot(Box::new(state.view(me)))))
  }
}

/// The view a client would render, for tests and the scripted run.
pub fn village_view(state: &VillageState, me: Option<PlayerId>) -> VillageView {
  state.view(me)
}
