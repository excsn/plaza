use crate::agent::{Agent, AgentId};
use crate::session::TargetedOp;
use crate::snapshot::SnapshotContext;
use async_trait::async_trait;
use std::time::Duration;

pub use crate::error::StateLogicError;

/// What drives a state change.
#[derive(Debug, Clone)]
pub enum LogicInput<Op, ID: AgentId> {
  /// Operations initiated by a specific agent.
  AgentOps { source: Agent<ID>, ops: Vec<Op> },
  /// A discrete step in time.
  TimeStep { delta_time: Duration },
  /// An agent joined, so it can be registered in state. The controller sends the
  /// joiner a snapshot immediately after this returns.
  AgentJoined { agent: Agent<ID> },
  /// An agent left, so it can be cleaned up.
  AgentLeft { agent_id: ID },
}

/// Asks the controller to re-send state to specific agents.
///
/// Each recipient's snapshot is built for that recipient, so this is how a game
/// pushes a changed view: a new hand dealt, a phase that reveals information.
#[derive(Debug, Clone)]
pub struct SnapshotRequest<ID: AgentId> {
  pub recipients: Vec<Agent<ID>>,
  pub context: Option<SnapshotContext>,
}

impl<ID: AgentId> SnapshotRequest<ID> {
  /// Re-snapshots these agents with the default (full) context.
  pub fn to(recipients: Vec<Agent<ID>>) -> Self {
    Self {
      recipients,
      context: None,
    }
  }

  /// Re-snapshots these agents under a named perspective.
  pub fn with_context(recipients: Vec<Agent<ID>>, context: SnapshotContext) -> Self {
    Self {
      recipients,
      context: Some(context),
    }
  }
}

/// What processing an input produced: ops to broadcast, and optionally state to
/// re-send.
///
/// `Vec<TargetedOp>` converts into this, so logic that only broadcasts ops ends
/// with `Ok(ops.into())`.
#[derive(Debug)]
pub struct LogicOutput<Op, ID: AgentId> {
  /// Operations to send to clients.
  pub ops: Vec<TargetedOp<Op, ID>>,
  /// Agents whose whole view changed and should be re-snapshotted.
  ///
  /// Applied after `ops`, so clients see the ops that explain the change before
  /// the state that reflects it.
  pub snapshots: Vec<SnapshotRequest<ID>>,
}

impl<Op, ID: AgentId> LogicOutput<Op, ID> {
  /// No ops, no snapshots.
  pub fn none() -> Self {
    Self {
      ops: Vec::new(),
      snapshots: Vec::new(),
    }
  }

  /// Just broadcast these ops.
  pub fn ops(ops: Vec<TargetedOp<Op, ID>>) -> Self {
    Self {
      ops,
      snapshots: Vec::new(),
    }
  }

  /// Also re-snapshot these agents, each getting a view built for them.
  pub fn and_snapshot(mut self, request: SnapshotRequest<ID>) -> Self {
    self.snapshots.push(request);
    self
  }
}

impl<Op, ID: AgentId> Default for LogicOutput<Op, ID> {
  fn default() -> Self {
    Self::none()
  }
}

impl<Op, ID: AgentId> From<Vec<TargetedOp<Op, ID>>> for LogicOutput<Op, ID> {
  fn from(ops: Vec<TargetedOp<Op, ID>>) -> Self {
    Self::ops(ops)
  }
}

/// The rules of your application: how `Op`s change `StateType`.
///
/// The only place state is mutated. The controller calls this one input at a
/// time from a single task, so no locking is needed inside.
#[async_trait]
pub trait StateLogic<Op, ID: AgentId, StateType>: Send + Sync + 'static {
  /// Applies `input` to `current_state` and reports what should follow.
  ///
  /// Return ops to broadcast, plus snapshot requests when a change alters what
  /// players may see:
  ///
  /// ```ignore
  /// // Ops only.
  /// Ok(ops.into())
  ///
  /// // A new round: tell everyone, then give each player their own new hand.
  /// Ok(LogicOutput::ops(ops).and_snapshot(SnapshotRequest::to(state.players())))
  /// ```
  ///
  /// `Err` means the input could not be applied; the controller logs it and
  /// carries on, so a rejected op does not stop the loop.
  async fn process_input(
    &self,
    current_state: &mut StateType,
    input: LogicInput<Op, ID>,
  ) -> Result<LogicOutput<Op, ID>, StateLogicError>;
}
