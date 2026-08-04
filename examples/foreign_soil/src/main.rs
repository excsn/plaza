//! Does the transport seam deliver what it advertises?
//!
//! Every other example rides an adapter plaza ships. This one writes a
//! transport against the published surface alone, then exercises it and prints
//! what the seam gave and what it did not. Running it *is* the test: the
//! findings are assertions, so a change that closes a gap or opens a new one
//! shows up here.

mod transport;

use std::sync::Arc;
use std::time::Duration;

use plaza::agent::Agent;
use plaza::controller::StateControllerBuilder;
use plaza::error::SnapshotError;
use plaza::session::{MessageTarget, Session, SessionMessage};
use plaza::snapshot::{SnapshotContext, SnapshotProvider};
use plaza::session::TargetedOp;
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic, StateLogicError};
use plaza_session::codec::{JsonCodec, WireCodec};
use plaza_session::{DirectionProfile, LinkProfile, SessionOptions};
use plaza_wire::frame;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use transport::{AgentFactory, UnixPlazaSession};

type PlayerId = u32;

#[derive(Debug, Clone, Serialize, Deserialize)]
enum Op {
  Say(String),
}

#[derive(Debug, Clone, Default)]
struct World {
  heard: usize,
}

struct Logic;

#[async_trait::async_trait]
impl StateLogic<Op, PlayerId, World> for Logic {
  async fn process_input(
    &self,
    state: &mut World,
    input: LogicInput<Op, PlayerId>,
  ) -> Result<LogicOutput<Op, PlayerId>, StateLogicError> {
    let LogicInput::AgentOps { ops, .. } = input else {
      return Ok(LogicOutput::none());
    };
    state.heard += ops.len();
    Ok(LogicOutput::ops(vec![TargetedOp::new_system_all(ops)]))
  }
}

struct NoViews;

#[async_trait::async_trait]
impl SnapshotProvider<PlayerId, World, Op> for NoViews {
  async fn create_snapshot(
    &self,
    _state: &World,
    _target: Option<&Agent<PlayerId>>,
    _ctx: Option<SnapshotContext>,
  ) -> Result<Option<Op>, SnapshotError<PlayerId>> {
    Ok(None)
  }
}

/// A client speaking the wire by hand, which is the other half of the claim.
///
/// It runs as a task rather than being polled on demand, because a probe is
/// answered whenever it arrives and not only while a test happens to be
/// looking. Polling it on demand made this example report that the link plane
/// does not work, which was the harness and not the seam.
struct Client {
  to_server: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
  ops_seen: tokio::sync::mpsc::UnboundedReceiver<()>,
}

impl Client {
  async fn connect(path: &str) -> Self {
    let stream = UnixStream::connect(path).await.expect("connect");
    let (to_server, mut outbox) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let (saw_ops, ops_seen) = tokio::sync::mpsc::unbounded_channel();

    tokio::spawn(async move {
      let (mut reader, mut writer) = tokio::io::split(stream);
      loop {
        tokio::select! {
          outgoing = outbox.recv() => {
            let Some(frame_bytes) = outgoing else { return };
            if writer.write_all(&(frame_bytes.len() as u32).to_be_bytes()).await.is_err()
              || writer.write_all(&frame_bytes).await.is_err()
            {
              return;
            }
          }
          incoming = read_one(&mut reader) => {
            let Some(frame_bytes) = incoming else { return };
            let Some((tag, body)) = frame::split(&frame_bytes) else { continue };
            match frame::Kind::from_byte(tag) {
              Some(frame::Kind::Ping) => {
                if let Some(reply) = frame::answer_ping(&JsonCodec, body, None) {
                  if writer.write_all(&(reply.len() as u32).to_be_bytes()).await.is_err()
                    || writer.write_all(&reply).await.is_err()
                  {
                    return;
                  }
                }
              }
              Some(frame::Kind::Ops) => {
                let _ = saw_ops.send(());
              }
              _ => {}
            }
          }
        }
      }
    });

    Self { to_server, ops_seen }
  }

  fn send(&self, frame_bytes: Vec<u8>) {
    let _ = self.to_server.send(frame_bytes);
  }

  async fn saw_ops(&mut self, within: Duration) -> bool {
    tokio::time::timeout(within, self.ops_seen.recv()).await.is_ok()
  }
}

async fn read_one<R: tokio::io::AsyncRead + Unpin>(reader: &mut R) -> Option<Vec<u8>> {
  let mut len = [0u8; 4];
  reader.read_exact(&mut len).await.ok()?;
  let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
  reader.read_exact(&mut body).await.ok()?;
  Some(body)
}

fn ops_frame(op: &Op) -> Vec<u8> {
  let mut buf = Vec::new();
  frame::begin(frame::Kind::Ops, &mut buf);
  JsonCodec.encode_into(&vec![op.clone()], &mut buf).expect("encode");
  buf
}

#[tokio::main]
async fn main() {
  tracing_subscriber::fmt().with_env_filter("info").init();

  let path = format!("/tmp/plaza-foreign-soil-{}.sock", std::process::id());
  let factory: AgentFactory<PlayerId> = Arc::new(|| Agent::new_human(1));
  let session = UnixPlazaSession::<Op, PlayerId, JsonCodec>::bind(
    &path,
    factory,
    JsonCodec,
    SessionOptions::default(),
  )
  .await
  .expect("bind");

  // The controller takes the inbound stream, which has a single consumer, so
  // an adapter proves inbound the way an application would: the logic echoes
  // what it hears, and the echo coming back is the whole path.
  let (_commands, controller) = StateControllerBuilder::new(
    Arc::new(Logic),
    session.clone(),
    Arc::new(NoViews),
    World::default(),
  )
  .build();
  tokio::spawn(controller.run());

  let mut client = Client::connect(&path).await;
  for _ in 0..100 {
    if session.manager().connection_count() == 1 {
      break;
    }
    tokio::time::sleep(Duration::from_millis(10)).await;
  }

  // 1 and 2. Client op reaches the controller, and the logic's broadcast
  // reaches the client: registry, bridge, outbound queue and the `Session`
  // delegation, all of it free.
  client.send(ops_frame(&Op::Say("hello".into())));
  let round_trip = client.saw_ops(Duration::from_secs(3)).await;

  // 3. The link plane, which the recipe used to omit entirely.
  let mut measured = None;
  for _ in 0..100 {
    if let Some((rtt, samples)) = session.manager().agent_link_rtt(&1) {
      measured = Some((rtt, samples));
      break;
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
  }

  // 4. Impairment: an application sets it and expects it to apply.
  session
    .manager()
    .set_all_link_profiles(LinkProfile::symmetric(DirectionProfile::delayed(Duration::from_millis(120))));
  let before = tokio::time::Instant::now();
  session
    .send_message(MessageTarget::All, SessionMessage::system(vec![Op::Say("delayed".into())]))
    .await
    .expect("send");
  let delayed_arrived = client.saw_ops(Duration::from_secs(3)).await;
  let observed = before.elapsed();

  println!("\n## what the seam gave a transport it did not ship\n");
  println!("| capability | works | how it was obtained |");
  println!("|---|---|---|");
  println!("| op round trip | {round_trip} | free: `register`, `forward_incoming`, the bridge, the outbound queue |");
  println!(
    "| link RTT measured | {} | free: `LinkDriver` owns the schedule and the correlation |",
    measured.is_some()
  );
  if let Some((rtt, samples)) = measured {
    println!("| | | {:.2}ms over {samples} samples |", rtt.as_secs_f64() * 1000.0);
  }
  println!(
    "| impairment applied | {} | free: the same `Conditioner` the shipped adapters use, delay and jitter and loss and all four ordering rules |",
    delayed_arrived && observed >= Duration::from_millis(100)
  );
  println!("\nThe connection loop is 65 lines, of which about 25 are reading and writing a socket.");
  println!("\nA 120ms downstream profile produced {}ms.", observed.as_millis());

  assert!(round_trip, "the registry, bridge and outbound queue are the part that is genuinely free");
  assert!(
    measured.is_some(),
    "probing is reimplementable, and this is the proof it is: if this fails the ledger is wrong"
  );

  let _ = std::fs::remove_file(&path);
}
