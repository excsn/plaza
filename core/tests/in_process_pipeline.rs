//! End-to-end coverage of the controller runtime over `InProcessSession`:
//! joins produce snapshots, client ops mutate state and broadcast, the tick
//! driver advances simulation time, and shutdown terminates the actor.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::controller::{ControllerCommand, StateControllerBuilder};
use plaza::error::SnapshotError;
use plaza::controller::CommandSender;
use plaza::session::in_process::ClientInbox;
use plaza::session::{InProcessSession, SessionMessage, TargetedOp};
use plaza::snapshot::{SnapshotContext, SnapshotProvider};
use plaza::state_logic::{LogicInput, LogicOutput, SnapshotRequest, StateLogic, StateLogicError};
use plaza::TickDriver;
use serde::{Deserialize, Serialize};
use fibre::oneshot;

type UserId = u64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum CounterOp {
  Increment(i64),
  /// Broadcast by the server after applying an increment.
  Changed(i64),
  /// Broadcast when an agent joins.
  Joined(UserId),
  /// A whole-state view, built per recipient. Boxed: unboxed, every `CounterOp`
  /// in every batch would be the size of a `CounterSnapshot`.
  Snapshot(Box<CounterSnapshot>),
}

#[derive(Debug, Clone, Default, PartialEq)]
struct CounterState {
  value: i64,
  ticks: u64,
  members: Vec<UserId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct CounterSnapshot {
  value: i64,
  members: Vec<UserId>,
}

#[derive(Debug, Default)]
struct CounterLogic;

#[async_trait]
impl StateLogic<CounterOp, UserId, CounterState> for CounterLogic {
  async fn process_input(
    &self,
    state: &mut CounterState,
    input: LogicInput<CounterOp, UserId>,
  ) -> Result<LogicOutput<CounterOp, UserId>, StateLogicError> {
    let mut out = Vec::new();

    match input {
      LogicInput::AgentOps { ops, .. } => {
        for op in ops {
          if let CounterOp::Increment(by) = op {
            state.value += by;
            out.push(TargetedOp::new_system_all(vec![CounterOp::Changed(state.value)]));
          }
        }
      }
      LogicInput::TimeStep { .. } => state.ticks += 1,
      LogicInput::AgentJoined { agent } => {
        if let Some(id) = agent.id_cloned() {
          state.members.push(id);
          out.push(TargetedOp::new_system_all(vec![CounterOp::Joined(id)]));
        }
      }
      LogicInput::AgentLeft { agent_id } => state.members.retain(|id| *id != agent_id),
    }

    Ok(out.into())
  }
}

/// A snapshot is an op now, so recognising one is a match on the op rather than
/// on a message kind.
fn snapshot_of(msg: &SessionMessage<CounterOp, UserId>) -> Option<&CounterSnapshot> {
  msg.ops.iter().find_map(|op| match op {
    CounterOp::Snapshot(snap) => Some(&**snap),
    _ => None,
  })
}

fn is_snapshot(msg: &SessionMessage<CounterOp, UserId>) -> bool {
  snapshot_of(msg).is_some()
}

#[derive(Debug, Default)]
struct CounterSnapshotter;

#[async_trait]
impl SnapshotProvider<UserId, CounterState, CounterOp> for CounterSnapshotter {
  async fn create_snapshot(
    &self,
    state: &CounterState,
    _target: Option<&Agent<UserId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<CounterOp>, SnapshotError<UserId>> {
    Ok(Some(CounterOp::Snapshot(Box::new(CounterSnapshot {
      value: state.value,
      members: state.members.clone(),
    }))))
  }
}

/// Spawns a controller wired to a fresh in-process session.
fn start() -> (
  Arc<InProcessSession<CounterOp, UserId>>,
  CommandSender<CounterOp, UserId, CounterState>,
) {
  let session = InProcessSession::<CounterOp, UserId>::new();
  let (tx, controller) = StateControllerBuilder::new(
    Arc::new(CounterLogic),
    session.clone(),
    Arc::new(CounterSnapshotter),
    CounterState::default(),
  )
  .command_buffer(64)
  .build();

  tokio::spawn(controller.run());
  (session, tx)
}

async fn query_state(tx: &CommandSender<CounterOp, UserId, CounterState>) -> CounterState {
  let (resp_tx, mut resp_rx) = oneshot::exclusive();
  tx.send(ControllerCommand::QueryCurrentState { response_tx: resp_tx })
    .await
    .expect("controller alive");
  resp_rx.recv().await.expect("controller responds")
}

/// Waits for a message satisfying `pred`, failing rather than hanging.
///
/// The session filters by target, so anything arriving here was addressed to
/// this client.
async fn recv_matching<F>(
  inbox: &ClientInbox<CounterOp, UserId>,
  pred: F,
) -> SessionMessage<CounterOp, UserId>
where
  F: Fn(&SessionMessage<CounterOp, UserId>) -> bool,
{
  tokio::time::timeout(Duration::from_secs(5), async {
    loop {
      let msg = inbox.recv().await.expect("session channel open");
      if pred(&msg) {
        return msg;
      }
    }
  })
  .await
  .expect("expected message did not arrive")
}

#[tokio::test]
async fn joining_agent_receives_a_snapshot() {
  let (session, tx) = start();

  let alice = Agent::new_human(1u64);
  let (_conn_id, inbox) = session.connect(alice).await.expect("connect");

  let msg = recv_matching(&inbox, |m| is_snapshot(m)).await;
  let snap = snapshot_of(&msg).expect("a snapshot op");
  assert_eq!(snap.members, vec![1u64], "snapshot reflects the join");

  assert_eq!(query_state(&tx).await.members, vec![1u64]);
}

#[tokio::test]
async fn client_op_mutates_state_and_is_broadcast() {
  let (session, tx) = start();

  let alice = Agent::new_human(1u64);
  let (_conn_id, inbox) = session.connect(alice.clone()).await.expect("connect");
  session.client_send(alice, vec![CounterOp::Increment(5)]).await;

  let msg = recv_matching(&inbox, |m| m.ops.contains(&CounterOp::Changed(5))).await;
  assert!(msg.ops.contains(&CounterOp::Changed(5)));

  assert_eq!(query_state(&tx).await.value, 5);
}

#[tokio::test]
async fn tick_driver_advances_simulation_time() {
  let (_session, tx) = start();

  TickDriver::new(Duration::from_millis(1)).run_for(tx.clone(), 5).await;

  assert_eq!(query_state(&tx).await.ticks, 5);
}

#[tokio::test]
async fn agent_leaving_is_reflected_in_state() {
  let (session, tx) = start();

  let alice = Agent::new_human(1u64);
  let (conn_id, _inbox) = session.connect(alice).await.expect("join");
  session.disconnect(&1u64, conn_id).await;

  // Both events are queued before the controller reads either. They share one
  // stream, so the leave cannot overtake the join, when they were separate
  // channels `select!` could pick either, and this test flaked.

  tokio::time::timeout(Duration::from_secs(5), async {
    loop {
      if query_state(&tx).await.members.is_empty() {
        return;
      }
      tokio::task::yield_now().await;
    }
  })
  .await
  .expect("member list should drain after the agent leaves");
}

#[tokio::test]
async fn shutdown_terminates_the_controller() {
  let session = InProcessSession::<CounterOp, UserId>::new();
  let (tx, controller) = StateControllerBuilder::new(
    Arc::new(CounterLogic),
    session,
    Arc::new(CounterSnapshotter),
    CounterState::default(),
  )
  .build();

  let handle = tokio::spawn(controller.run());
  tx.send(ControllerCommand::Shutdown).await.expect("controller alive");

  let result = tokio::time::timeout(Duration::from_secs(5), handle)
    .await
    .expect("controller should stop promptly")
    .expect("controller task should not panic");
  assert!(result.is_ok());
}

/// A snapshotter that hides other players' values, the way a card game hides
/// hands. Proves the provider can return a different payload per recipient.
#[derive(Debug, Default)]
struct PerPlayerSnapshotter;

#[async_trait]
impl SnapshotProvider<UserId, CounterState, CounterOp> for PerPlayerSnapshotter {
  async fn create_snapshot(
    &self,
    state: &CounterState,
    target: Option<&Agent<UserId>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<CounterOp>, SnapshotError<UserId>> {
    // Each player sees only themselves in `members`.
    let me = target.and_then(|a| a.id_cloned());
    Ok(Some(CounterOp::Snapshot(Box::new(CounterSnapshot {
      value: state.value,
      members: me.into_iter().collect(),
    }))))
  }
}

#[tokio::test]
async fn each_agent_receives_a_snapshot_built_for_it() {
  let session = InProcessSession::<CounterOp, UserId>::new();
  let (tx, controller) = StateControllerBuilder::new(
    Arc::new(CounterLogic),
    session.clone(),
    Arc::new(PerPlayerSnapshotter),
    CounterState::default(),
  )
  .build();
  tokio::spawn(controller.run());

  let alice = Agent::new_human(1u64);
  let bob = Agent::new_human(2u64);
  let (_a_conn, alice_inbox) = session.connect(alice.clone()).await.expect("connect alice");
  let (_b_conn, bob_inbox) = session.connect(bob.clone()).await.expect("connect bob");

  // Drain each client's join snapshot before asking for a fresh round.
  for inbox in [&alice_inbox, &bob_inbox] {
    recv_matching(inbox, |m| is_snapshot(m)).await;
  }

  tx.send(ControllerCommand::SendSnapshots {
    recipients: vec![alice, bob],
    context: Some(SnapshotContext::ForPerspective("player".into())),
  })
  .await
  .expect("controller alive");

  let for_alice = recv_matching(&alice_inbox, |m| is_snapshot(m)).await;
  let for_bob = recv_matching(&bob_inbox, |m| is_snapshot(m)).await;

  fn members(msg: SessionMessage<CounterOp, UserId>) -> Vec<UserId> {
    match snapshot_of(&msg) {
      Some(snap) => snap.members.clone(),
      None => panic!("expected a snapshot op, got {:?}", msg),
    }
  }
  assert_eq!(members(for_alice), vec![1u64], "Alice sees only herself");
  assert_eq!(members(for_bob), vec![2u64], "Bob sees only himself");
}

/// Logic that re-snapshots every member when the counter crosses a threshold,
/// standing in for a phase change that alters what players may see.
#[derive(Debug, Default)]
struct ResnapshottingLogic;

#[async_trait]
impl StateLogic<CounterOp, UserId, CounterState> for ResnapshottingLogic {
  async fn process_input(
    &self,
    state: &mut CounterState,
    input: LogicInput<CounterOp, UserId>,
  ) -> Result<LogicOutput<CounterOp, UserId>, StateLogicError> {
    match input {
      LogicInput::AgentJoined { agent } => {
        if let Some(id) = agent.id_cloned() {
          state.members.push(id);
        }
        Ok(LogicOutput::none())
      }
      LogicInput::AgentOps { ops, .. } => {
        for op in ops {
          if let CounterOp::Increment(by) = op {
            state.value += by;
          }
        }
        // The view changed for everyone; push it rather than waiting to be asked.
        let everyone: Vec<_> = state
          .members
          .iter()
          .map(|id| Agent::new_human(*id))
          .collect();
        Ok(LogicOutput::none().and_snapshot(SnapshotRequest::to(everyone)))
      }
      _ => Ok(LogicOutput::none()),
    }
  }
}

#[tokio::test]
async fn logic_can_push_a_resnapshot_to_every_player() {
  let session = InProcessSession::<CounterOp, UserId>::new();
  let (_tx, controller) = StateControllerBuilder::new(
    Arc::new(ResnapshottingLogic),
    session.clone(),
    Arc::new(CounterSnapshotter),
    CounterState::default(),
  )
  .build();
  tokio::spawn(controller.run());

  let alice = Agent::new_human(1u64);
  let bob = Agent::new_human(2u64);
  let (_a, alice_inbox) = session.connect(alice.clone()).await.expect("connect alice");
  let (_b, bob_inbox) = session.connect(bob).await.expect("connect bob");

  // Drain the join snapshots.
  for inbox in [&alice_inbox, &bob_inbox] {
    recv_matching(inbox, |m| is_snapshot(m)).await;
  }

  // One client's op triggers a resnapshot for everyone, from inside the logic.
  session.client_send(alice, vec![CounterOp::Increment(3)]).await;

  for inbox in [&alice_inbox, &bob_inbox] {
    let msg = recv_matching(inbox, |m| is_snapshot(m)).await;
    assert_eq!(snapshot_of(&msg).expect("a snapshot op").value, 3);
  }
}

#[tokio::test]
async fn the_join_snapshot_context_is_configurable() {
  /// Reports which context it was handed, so the join path can be observed.
  #[derive(Debug, Default)]
  struct ContextReportingSnapshotter;

  #[async_trait]
  impl SnapshotProvider<UserId, CounterState, CounterOp> for ContextReportingSnapshotter {
    async fn create_snapshot(
      &self,
      _state: &CounterState,
      _target: Option<&Agent<UserId>>,
      context: Option<SnapshotContext>,
    ) -> Result<Option<CounterOp>, SnapshotError<UserId>> {
      // Encode the perspective name in `value` so the test can assert on it.
      let value = match context {
        Some(SnapshotContext::ForPerspective(name)) if name == "spectator" => 99,
        _ => 0,
      };
      Ok(Some(CounterOp::Snapshot(Box::new(CounterSnapshot { value, members: vec![] }))))
    }
  }

  let session = InProcessSession::<CounterOp, UserId>::new();
  let (_tx, controller) = StateControllerBuilder::new(
    Arc::new(CounterLogic),
    session.clone(),
    Arc::new(ContextReportingSnapshotter),
    CounterState::default(),
  )
  .snapshot_context_on_join(Some(SnapshotContext::ForPerspective("spectator".into())))
  .build();
  tokio::spawn(controller.run());

  let (_conn, inbox) = session
    .connect(Agent::new_human(1u64))
    .await
    .expect("connect");

  let msg = recv_matching(&inbox, |m| is_snapshot(m)).await;
  let snap = snapshot_of(&msg).expect("a snapshot op");
  assert_eq!(snap.value, 99, "join used the configured perspective");
}

#[tokio::test]
async fn shutdown_drains_work_queued_before_it() {
  let session = InProcessSession::<CounterOp, UserId>::new();
  let (tx, controller) = StateControllerBuilder::new(
    Arc::new(CounterLogic),
    session,
    Arc::new(CounterSnapshotter),
    CounterState::default(),
  )
  .build();
  let handle = tokio::spawn(controller.run());

  // Queue work, then immediately ask to stop. The increments were submitted
  // first, so they must be applied before the controller exits.
  let alice = Agent::new_human(1u64);
  for _ in 0..5 {
    tx.send(ControllerCommand::SubmitAgentOps {
      agent: alice.clone(),
      ops: vec![CounterOp::Increment(2)],
    })
    .await
    .expect("controller alive");
  }
  tx.send(ControllerCommand::Shutdown).await.expect("controller alive");

  let final_state = tokio::time::timeout(Duration::from_secs(5), handle)
    .await
    .expect("controller should stop promptly")
    .expect("controller task should not panic")
    .expect("run should succeed");

  assert_eq!(
    final_state.value, 10,
    "all five increments were applied before shutting down"
  );
}

#[tokio::test]
async fn run_returns_the_final_state_for_the_caller_to_persist() {
  let session = InProcessSession::<CounterOp, UserId>::new();
  let (tx, controller) = StateControllerBuilder::new(
    Arc::new(CounterLogic),
    session.clone(),
    Arc::new(CounterSnapshotter),
    CounterState::default(),
  )
  .build();
  let handle = tokio::spawn(controller.run());

  let alice = Agent::new_human(1u64);
  session.connect(alice.clone()).await.expect("connect");
  session.client_send(alice, vec![CounterOp::Increment(7)]).await;

  // Wait for the op to land before stopping.
  tokio::time::timeout(Duration::from_secs(5), async {
    while query_state(&tx).await.value != 7 {
      tokio::task::yield_now().await;
    }
  })
  .await
  .expect("op should be applied");

  tx.send(ControllerCommand::Shutdown).await.expect("controller alive");
  let final_state = handle.await.expect("no panic").expect("run should succeed");

  assert_eq!(final_state.value, 7, "the caller gets the state back to persist");
  assert_eq!(final_state.members, vec![1u64]);
}

#[tokio::test]
async fn an_application_defined_context_survives_the_round_trip() {
  /// A version scheme plaza knows nothing about, deliberately not a `u64`.
  #[derive(Debug, Clone, PartialEq)]
  struct SinceDigest(String);

  /// Answers only when handed a `SinceDigest`, proving the context arrived
  /// intact and typed.
  #[derive(Debug, Default)]
  struct DigestAwareSnapshotter;

  #[async_trait]
  impl SnapshotProvider<UserId, CounterState, CounterOp> for DigestAwareSnapshotter {
    async fn create_snapshot(
      &self,
      state: &CounterState,
      _target: Option<&Agent<UserId>>,
      context: Option<SnapshotContext>,
    ) -> Result<Option<CounterOp>, SnapshotError<UserId>> {
      let value = match context.as_ref().and_then(SnapshotContext::downcast_ref::<SinceDigest>) {
        Some(SinceDigest(d)) if d == "abc123" => 42,
        _ => state.value,
      };
      Ok(Some(CounterOp::Snapshot(Box::new(CounterSnapshot { value, members: vec![] }))))
    }
  }

  let session = InProcessSession::<CounterOp, UserId>::new();
  let (tx, controller) = StateControllerBuilder::new(
    Arc::new(CounterLogic),
    session.clone(),
    Arc::new(DigestAwareSnapshotter),
    CounterState::default(),
  )
  .build();
  tokio::spawn(controller.run());

  let alice = Agent::new_human(1u64);
  let (_conn, inbox) = session.connect(alice.clone()).await.expect("connect");
  recv_matching(&inbox, |m| is_snapshot(m)).await;

  tx.send(ControllerCommand::SendSnapshots {
    recipients: vec![alice],
    context: Some(SnapshotContext::custom(SinceDigest("abc123".into()))),
  })
  .await
  .expect("controller alive");

  let msg = recv_matching(&inbox, |m| is_snapshot(m)).await;
  let snap = snapshot_of(&msg).expect("a snapshot op");
  assert_eq!(snap.value, 42, "the provider received its own context type");
}
