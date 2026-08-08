//! The run, built once and sent to everyone: open information, one uniform
//! view. The per-seat `acked_seq` rides in it, which is the ack half of the
//! exactly-once machinery: a client trims its outbox against its own seat.

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::error::SnapshotError;
use plaza::snapshot::{SnapshotContext, SnapshotProvider};

use crate::protocol::{PlayerId, RunOp};
use crate::state::RunState;

#[derive(Debug, Default)]
pub struct RunSnapshotter;

#[async_trait]
impl SnapshotProvider<PlayerId, RunState, RunOp> for RunSnapshotter {
  async fn create_snapshot(
    &self,
    state: &RunState,
    _target_agent: Option<&Agent<PlayerId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<RunOp>, SnapshotError<PlayerId>> {
    Ok(Some(RunOp::Snapshot(Box::new(state.view()))))
  }
}
