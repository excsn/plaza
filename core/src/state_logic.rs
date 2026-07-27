use crate::agent::{Agent, AgentId};
use crate::session::TargetedOp;
use crate::snapshot::SnapshotContext;
use async_trait::async_trait;
use std::fmt;
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

impl<Op, ID: AgentId> LogicInput<Op, ID> {
  /// The variant name, for grouping in logs and metrics.
  ///
  /// A `&'static str`, so it is free to capture where a formatted description
  /// would not be: span fields are recorded eagerly, and this runs every tick.
  pub fn kind(&self) -> &'static str {
    match self {
      LogicInput::AgentOps { .. } => "AgentOps",
      LogicInput::TimeStep { .. } => "TimeStep",
      LogicInput::AgentJoined { .. } => "AgentJoined",
      LogicInput::AgentLeft { .. } => "AgentLeft",
    }
  }
}

/// Allocation-free, so a `debug!` that is switched off costs nothing.
impl<Op, ID: AgentId> fmt::Display for LogicInput<Op, ID> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      LogicInput::AgentOps { source, ops } => write!(f, "AgentOps({source}, {} ops)", ops.len()),
      LogicInput::TimeStep { delta_time } => write!(f, "TimeStep({delta_time:?})"),
      LogicInput::AgentJoined { agent } => write!(f, "AgentJoined({agent})"),
      LogicInput::AgentLeft { agent_id } => write!(f, "AgentLeft({agent_id:?})"),
    }
  }
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

  /// Merges neighbouring ops that share a sender and a target into one entry.
  ///
  /// The controller sends one envelope per `TargetedOp`, and logic naturally
  /// pushes one per event, so a tick that hid a mole and spawned another sent
  /// two frames to the same everyone: two encodes, two fan-outs, and two copies
  /// of an envelope that is around 34 bytes of JSON wrapped around ops often
  /// smaller than that. The controller calls this before sending.
  ///
  /// **Neighbours only, and that is the whole subtlety.** Merging across a gap
  /// reorders: given `[A→all, B→p1, C→all]`, folding `C` into `A` moves it
  /// ahead of `B` for the one recipient that receives both. Restricting it to
  /// runs means any two ops that can reach a common recipient keep the order
  /// logic emitted them in, which is the only ordering guarantee ops have.
  pub fn coalesce(&mut self) {
    // `dedup_by` passes the later element first and drops it when the closure
    // says yes, which is exactly a fold into the run's surviving head.
    self.ops.dedup_by(|current, kept| {
      if kept.from_agent == current.from_agent && kept.target == current.target {
        kept.ops.append(&mut current.ops);
        true
      } else {
        false
      }
    });
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

#[cfg(test)]
mod tests {
  use super::*;
  use crate::session::MessageTarget;

  fn to(target: MessageTarget<u64>, ops: &[u8]) -> TargetedOp<u8, u64> {
    TargetedOp::new(Agent::system(), target, ops.to_vec())
  }

  fn shape(output: &LogicOutput<u8, u64>) -> Vec<(MessageTarget<u64>, Vec<u8>)> {
    output
      .ops
      .iter()
      .map(|t| (t.target.clone(), t.ops.clone()))
      .collect()
  }

  #[test]
  fn a_run_of_same_target_ops_becomes_one_message() {
    let mut output = LogicOutput::ops(vec![
      to(MessageTarget::All, &[1]),
      to(MessageTarget::All, &[2]),
      to(MessageTarget::All, &[3]),
    ]);
    output.coalesce();
    assert_eq!(shape(&output), vec![(MessageTarget::All, vec![1, 2, 3])]);
  }

  #[test]
  fn coalescing_never_reorders_what_a_recipient_sees() {
    // The constraint that makes this neighbours-only. `p1` receives every one
    // of these, so folding the second `All` into the first would move op 3
    // ahead of op 2 for them.
    let mut output = LogicOutput::ops(vec![
      to(MessageTarget::All, &[1]),
      to(MessageTarget::Agent(1), &[2]),
      to(MessageTarget::All, &[3]),
    ]);
    output.coalesce();
    assert_eq!(
      shape(&output),
      vec![
        (MessageTarget::All, vec![1]),
        (MessageTarget::Agent(1), vec![2]),
        (MessageTarget::All, vec![3]),
      ],
      "ops that can reach a common recipient must keep their emitted order"
    );
  }

  #[test]
  fn a_different_sender_breaks_the_run() {
    // `from` is on the envelope, so two senders cannot share one.
    let mut output = LogicOutput::ops(vec![
      TargetedOp::new(Agent::system(), MessageTarget::All, vec![1u8]),
      TargetedOp::new(Agent::new_human(7u64), MessageTarget::All, vec![2u8]),
      TargetedOp::new(Agent::new_human(7u64), MessageTarget::All, vec![3u8]),
    ]);
    output.coalesce();
    assert_eq!(output.ops.len(), 2);
    assert_eq!(output.ops[0].ops, vec![1]);
    assert_eq!(output.ops[1].ops, vec![2, 3]);
  }

  #[test]
  fn coalescing_nothing_is_harmless() {
    let mut empty: LogicOutput<u8, u64> = LogicOutput::none();
    empty.coalesce();
    assert!(empty.ops.is_empty());

    let mut single = LogicOutput::ops(vec![to(MessageTarget::All, &[1])]);
    single.coalesce();
    assert_eq!(shape(&single), vec![(MessageTarget::All, vec![1])]);
  }

  #[test]
  fn an_input_describes_itself_without_allocating_on_the_tick_path() {
    // Both of these used to be built eagerly, before the `debug!` that consumed
    // them, so every tick paid for a string the log level then discarded.
    let ops: LogicInput<u8, u64> = LogicInput::AgentOps {
      source: Agent::new_human(7),
      ops: vec![1, 2, 3],
    };
    assert_eq!(ops.to_string(), "AgentOps(human:7, 3 ops)");
    assert_eq!(ops.kind(), "AgentOps");

    let tick: LogicInput<u8, u64> = LogicInput::TimeStep {
      delta_time: Duration::from_millis(16),
    };
    assert_eq!(tick.to_string(), "TimeStep(16ms)");
    assert_eq!(tick.kind(), "TimeStep");

    let joined: LogicInput<u8, u64> = LogicInput::AgentJoined {
      agent: Agent::new_bot(2),
    };
    assert_eq!(joined.to_string(), "AgentJoined(bot:2)");

    let left: LogicInput<u8, u64> = LogicInput::AgentLeft { agent_id: 9u64 };
    assert_eq!(left.to_string(), "AgentLeft(9)");
  }
}
