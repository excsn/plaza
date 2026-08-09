//! The `OpGuard` seam over `InProcessSession`: a refused op never reaches
//! `StateLogic`, its reply reaches the source, system submissions bypass the
//! guard, and a batch mixes cleared and refused per op.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::controller::{CommandSender, ControllerCommand, StateControllerBuilder};
use plaza::op_guard::{GuardFn, OpClearance};
use plaza::session::in_process::ClientInbox;
use plaza::session::{InProcessSession, SessionMessage, TargetedOp};
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic, StateLogicError};

type UserId = u64;

#[derive(Debug, Clone, PartialEq, Eq)]
enum VaultOp {
  Deposit(u64),
  Withdraw(u64),
  Balance(u64),
  Refused,
}

#[derive(Debug, Default)]
struct Vault {
  balance: u64,
}

/// Counts every batch the logic sees, so a test can assert it saw none.
#[derive(Debug, Default)]
struct VaultLogic {
  batches: AtomicU64,
}

#[async_trait]
impl StateLogic<VaultOp, UserId, Vault> for VaultLogic {
  async fn process_input(
    &self,
    state: &mut Vault,
    input: LogicInput<VaultOp, UserId>,
  ) -> Result<LogicOutput<VaultOp, UserId>, StateLogicError> {
    let mut out = Vec::new();
    if let LogicInput::AgentOps { ops, .. } = input {
      self.batches.fetch_add(1, Ordering::Relaxed);
      for op in ops {
        match op {
          VaultOp::Deposit(amount) => state.balance += amount,
          VaultOp::Withdraw(amount) => state.balance = state.balance.saturating_sub(amount),
          _ => continue,
        }
        out.push(TargetedOp::new_system_all(vec![VaultOp::Balance(state.balance)]));
      }
    }
    Ok(out.into())
  }
}

/// Withdrawals need standing the state can refuse; deposits are anyone's.
fn no_overdraft(state: &Vault, _source: &Agent<UserId>, op: &VaultOp) -> OpClearance<VaultOp> {
  match op {
    VaultOp::Withdraw(amount) if *amount > state.balance => OpClearance::Refused {
      reply: Some(VaultOp::Refused),
    },
    _ => OpClearance::Cleared,
  }
}

fn start() -> (
  Arc<InProcessSession<VaultOp, UserId>>,
  CommandSender<VaultOp, UserId, Vault>,
  Arc<VaultLogic>,
  Arc<plaza::stats::ControllerStats>,
) {
  let session = InProcessSession::<VaultOp, UserId>::new();
  let logic = Arc::new(VaultLogic::default());
  let builder = StateControllerBuilder::without_snapshots(logic.clone(), session.clone(), Vault::default())
    .guard(Arc::new(GuardFn(no_overdraft)));
  let stats = builder.stats();
  let (tx, controller) = builder.build();
  tokio::spawn(controller.run());
  (session, tx, logic, stats)
}

async fn recv_matching<F>(inbox: &ClientInbox<VaultOp, UserId>, pred: F) -> SessionMessage<VaultOp, UserId>
where
  F: Fn(&SessionMessage<VaultOp, UserId>) -> bool,
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

async fn balance(tx: &CommandSender<VaultOp, UserId, Vault>) -> u64 {
  plaza::controller::query_with(tx, |vault| vault.balance)
    .await
    .expect("controller alive")
}

#[tokio::test]
async fn a_refused_op_never_reaches_the_logic_and_its_reply_reaches_the_source() {
  let (session, tx, logic, stats) = start();
  let alice = Agent::new_human(1u64);
  let (_conn, inbox) = session.connect(alice.clone()).await.expect("connect");

  session.client_send(alice, vec![VaultOp::Withdraw(10)]).await;
  recv_matching(&inbox, |m| m.ops.contains(&VaultOp::Refused)).await;

  assert_eq!(balance(&tx).await, 0);
  assert_eq!(logic.batches.load(Ordering::Relaxed), 0, "a fully refused batch is not delivered");
  assert_eq!(stats.ops_refused(), 1);
}

#[tokio::test]
async fn a_batch_is_screened_per_op_not_as_a_whole() {
  let (session, tx, _logic, stats) = start();
  let alice = Agent::new_human(1u64);
  let (_conn, inbox) = session.connect(alice.clone()).await.expect("connect");

  session
    .client_send(alice, vec![VaultOp::Deposit(5), VaultOp::Withdraw(3)])
    .await;
  recv_matching(&inbox, |m| m.ops.contains(&VaultOp::Refused)).await;
  recv_matching(&inbox, |m| m.ops.contains(&VaultOp::Balance(5))).await;

  // The whole batch is screened against the state it arrived to: the deposit
  // riding in front has not applied yet, so even an affordable withdrawal
  // behind it is refused. The deposit itself clears and lands.
  assert_eq!(balance(&tx).await, 5);
  assert_eq!(stats.ops_refused(), 1);
}

#[tokio::test]
async fn system_submissions_bypass_the_guard() {
  let (_session, tx, _logic, stats) = start();

  tx.send(ControllerCommand::SubmitSystemOps {
    source_description: "test".into(),
    ops: vec![VaultOp::Withdraw(10)],
  })
  .await
  .expect("controller alive");

  assert_eq!(balance(&tx).await, 0, "saturating_sub applied it; the guard never saw it");
  assert_eq!(stats.ops_refused(), 0);
}
