//! A client that speaks the wire by hand, so the door can be knocked on.
//!
//! Answers the session's probes, so a client who says nothing still has a
//! live, measured link and keeps the goodbye, which is where every refusal
//! and every close says why.

use std::sync::Arc;

use parking_lot::Mutex;
use plaza_session::codec::JsonCodec;
use plaza_session::DEFAULT_MAX_FRAME_BYTES;
use plaza_wire::frame::{self, Goodbye, Kind};
use plaza_wire::framing::{delimit, LengthDelimited};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::types::{decode_ops, encode_ops, Account, ArcadeOp, Refusal};

pub struct Knock {
  pub heard: Arc<Mutex<Vec<ArcadeOp>>>,
  goodbye: Arc<Mutex<Option<Goodbye>>>,
  writer: Arc<tokio::sync::Mutex<tokio::io::WriteHalf<TcpStream>>>,
  task: tokio::task::JoinHandle<()>,
}

impl Knock {
  /// Connects, presents the account as its credential if it has one, and
  /// collects whatever it is told. `None` presents nothing, which is what a
  /// half-open flood looks like.
  pub async fn arrive(addr: &str, account: Option<Account>) -> std::io::Result<Self> {
    let stream = TcpStream::connect(addr).await?;
    let heard = Arc::new(Mutex::new(Vec::new()));
    let goodbye = Arc::new(Mutex::new(None));

    let (mut read_half, mut write_half) = tokio::io::split(stream);
    if let Some(account) = account {
      let mut credential = Vec::new();
      frame::begin(Kind::Credential, &mut credential);
      credential.extend_from_slice(account.to_string().as_bytes());
      let mut wire = Vec::new();
      delimit(&credential, &mut wire);
      write_half.write_all(&wire).await?;
    }

    let writer = Arc::new(tokio::sync::Mutex::new(write_half));
    let sink = heard.clone();
    let farewell = goodbye.clone();
    let pong_writer = writer.clone();
    let task = tokio::spawn(async move {
      let mut framing = LengthDelimited::new(DEFAULT_MAX_FRAME_BYTES);
      let mut chunk = [0u8; 8192];
      loop {
        while let Ok(Some(frame)) = framing.next_frame() {
          match frame.first().copied().and_then(Kind::from_byte) {
            Some(Kind::Ping) => {
              if let Some(reply) = frame::answer_ping(&JsonCodec, &frame[1..], None) {
                let mut wire = Vec::new();
                delimit(&reply, &mut wire);
                let _ = pong_writer.lock().await.write_all(&wire).await;
              }
            }
            Some(Kind::Goodbye) => {
              *farewell.lock() = frame::decode_goodbye(&JsonCodec, &frame);
            }
            Some(Kind::Ops) => sink.lock().extend(decode_ops(&frame)),
            _ => {}
          }
        }
        match read_half.read(&mut chunk).await {
          Ok(0) | Err(_) => return,
          Ok(n) => framing.feed(&chunk[..n]),
        }
      }
    });

    Ok(Self {
      heard,
      goodbye,
      writer,
      task,
    })
  }

  pub fn goodbye(&self) -> Option<Goodbye> {
    self.goodbye.lock().clone()
  }

  /// Why the door said no, for a connection that never got in.
  pub fn refusal(&self) -> Option<Refusal> {
    if self.was_admitted() {
      return None;
    }
    self.goodbye().and_then(|g| Refusal::from_code(g.code))
  }

  pub fn was_admitted(&self) -> bool {
    self
      .heard
      .lock()
      .iter()
      .any(|op| matches!(op, ArcadeOp::Admitted { .. }))
  }

  /// Why a session that was inside ended, from its goodbye's detail.
  pub fn closure(&self) -> Option<String> {
    if !self.was_admitted() {
      return None;
    }
    self
      .goodbye()
      .and_then(|g| g.detail)
      .map(|detail| String::from_utf8_lossy(&detail).into_owned())
  }

  /// Whether the socket was closed for presenting nothing in time.
  pub fn timed_out(&self) -> bool {
    self.goodbye().is_some_and(|g| g.code == Goodbye::CREDENTIAL_TIMEOUT)
  }

  pub fn snapshots(&self) -> usize {
    self
      .heard
      .lock()
      .iter()
      .filter(|op| matches!(op, ArcadeOp::Snapshot(_)))
      .count()
  }

  /// Sends ops, which is also how a closed connection proves it is closed.
  pub async fn say(&self, ops: &[ArcadeOp]) -> std::io::Result<()> {
    let mut wire = Vec::new();
    delimit(&encode_ops(ops), &mut wire);
    self.writer.lock().await.write_all(&wire).await
  }

  pub fn leave(self) {
    self.task.abort();
  }
}
