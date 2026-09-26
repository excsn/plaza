//! A producer blocked on a full controller or session channel resumes as soon
//! as the controller takes an item, not after the channel drains further.
//! fibre 0.6.4 held blocked senders after a single-item `recv` until
//! `capacity` items were taken or the channel emptied.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::controller::{CommandSender, ControllerCommand, StateControllerBuilder};
use plaza::session::InProcessSession;
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic, StateLogicError};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::Semaphore;

type UserId = u64;

/// Holds each op batch until the test releases it, so the queues in front of
/// the controller fill and stay full.
struct GatedLogic {
  entered: UnboundedSender<u32>,
  gate: Arc<Semaphore>,
}

#[async_trait]
impl StateLogic<u32, UserId, ()> for GatedLogic {
  async fn process_input(
    &self,
    _state: &mut (),
    input: LogicInput<u32, UserId>,
  ) -> Result<LogicOutput<u32, UserId>, StateLogicError> {
    if let LogicInput::AgentOps { ops, .. } = input {
      for op in ops {
        let _ = self.entered.send(op);
        self.gate.acquire().await.expect("gate open").forget();
      }
    }
    Ok(Vec::new().into())
  }
}

struct Harness {
  session: Arc<InProcessSession<u32, UserId>>,
  tx: CommandSender<u32, UserId, ()>,
  entered: UnboundedReceiver<u32>,
  gate: Arc<Semaphore>,
}

fn start(command_buffer: usize, session_capacity: usize) -> Harness {
  let (entered_tx, entered) = unbounded_channel();
  let gate = Arc::new(Semaphore::new(0));
  let session = InProcessSession::<u32, UserId>::with_capacity(session_capacity, 16);
  let logic = GatedLogic {
    entered: entered_tx,
    gate: gate.clone(),
  };
  let (tx, controller) = StateControllerBuilder::without_snapshots(Arc::new(logic), session.clone(), ())
    .command_buffer(command_buffer)
    .build();
  tokio::spawn(controller.run());
  Harness {
    session,
    tx,
    entered,
    gate,
  }
}

async fn entered(harness: &mut Harness) -> u32 {
  tokio::time::timeout(Duration::from_secs(1), harness.entered.recv())
    .await
    .expect("controller took the next op")
    .expect("logic alive")
}

fn system_op(op: u32) -> ControllerCommand<u32, UserId, ()> {
  ControllerCommand::SubmitSystemOps {
    source_description: "backpressure".into(),
    ops: vec![op],
  }
}

/// Fails unless the producer is parked: nothing acknowledged while the queue is
/// full.
async fn assert_blocked(ack_rx: &mut UnboundedReceiver<u32>) {
  let ack = tokio::time::timeout(Duration::from_millis(20), ack_rx.recv()).await;
  assert!(ack.is_err(), "producer sent into a full queue: {ack:?}");
}

async fn stop(harness: Harness) {
  harness.gate.add_permits(1 << 16);
  let _ = harness.tx.send(ControllerCommand::Shutdown).await;
}

#[tokio::test]
async fn blocked_command_sender_resumes_per_command_taken() {
  const CAP: u32 = 4;
  const ROUNDS: u32 = 2 * CAP;
  let mut harness = start(CAP as usize, 16);

  harness.tx.send(system_op(0)).await.unwrap();
  assert_eq!(entered(&mut harness).await, 0);
  for op in 1..=CAP {
    harness.tx.try_send(system_op(op)).expect("queue has room");
  }

  let (ack_tx, mut ack_rx) = unbounded_channel();
  let producer = harness.tx.clone();
  tokio::spawn(async move {
    for op in CAP + 1..=CAP + ROUNDS {
      if producer.send(system_op(op)).await.is_err() {
        return;
      }
      let _ = ack_tx.send(op);
    }
  });

  for r in 0..ROUNDS {
    assert_blocked(&mut ack_rx).await;
    harness.gate.add_permits(1);
    assert_eq!(entered(&mut harness).await, r + 1);
    let ack = tokio::time::timeout(Duration::from_secs(1), ack_rx.recv()).await;
    assert_eq!(ack, Ok(Some(CAP + 1 + r)), "round {r}: command sender stayed blocked with a free slot");
  }

  stop(harness).await;
}

#[tokio::test]
async fn blocked_client_send_resumes_per_batch_taken() {
  const CAP: u32 = 16;
  const BATCH: u32 = 8;
  let mut harness = start(16, CAP as usize);
  let client = Agent::new_human(1u64);

  harness.session.client_send(client.clone(), vec![0]).await;
  assert_eq!(entered(&mut harness).await, 0);
  for op in 1..=CAP {
    harness.session.client_send(client.clone(), vec![op]).await;
  }

  let (ack_tx, mut ack_rx) = unbounded_channel();
  let session = harness.session.clone();
  tokio::spawn(async move {
    session.client_send(client, vec![CAP + 1]).await;
    let _ = ack_tx.send(CAP + 1);
  });

  assert_blocked(&mut ack_rx).await;
  harness.gate.add_permits(1);
  assert_eq!(entered(&mut harness).await, 1);
  let ack = tokio::time::timeout(Duration::from_secs(1), ack_rx.recv()).await;
  assert_eq!(
    ack,
    Ok(Some(CAP + 1)),
    "client send stayed blocked after the controller took a batch of {BATCH}"
  );

  stop(harness).await;
}
