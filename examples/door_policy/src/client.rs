//! A client that speaks the wire by hand, so the door can be knocked on.
//!
//! Answers the session's probes, so a client who says nothing still has a
//! live, measured link.

use std::sync::Arc;

use parking_lot::Mutex;
use plaza_session::codec::JsonCodec;
use plaza_session::DEFAULT_MAX_FRAME_BYTES;
use plaza_wire::frame::Kind;
use plaza_wire::framing::{delimit, LengthDelimited};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::types::{decode_ops, encode_ops, Account, ArcadeOp, Refusal};

pub struct Knock {
  pub heard: Arc<Mutex<Vec<ArcadeOp>>>,
  writer: Arc<tokio::sync::Mutex<tokio::io::WriteHalf<TcpStream>>>,
  task: tokio::task::JoinHandle<()>,
}

impl Knock {
  /// Connects, says who it is, and collects whatever it is told.
  pub async fn arrive(addr: &str, account: Option<Account>) -> std::io::Result<Self> {
    let stream = TcpStream::connect(addr).await?;
    let heard = Arc::new(Mutex::new(Vec::new()));

    let (mut read_half, mut write_half) = tokio::io::split(stream);
    if let Some(account) = account {
      let mut wire = Vec::new();
      delimit(&encode_ops(&[ArcadeOp::Hello { account }]), &mut wire);
      write_half.write_all(&wire).await?;
    }

    let writer = Arc::new(tokio::sync::Mutex::new(write_half));
    let sink = heard.clone();
    let pong_writer = writer.clone();
    let task = tokio::spawn(async move {
      let mut framing = LengthDelimited::new(DEFAULT_MAX_FRAME_BYTES);
      let mut chunk = [0u8; 8192];
      loop {
        while let Ok(Some(frame)) = framing.next_frame() {
          if frame.first().copied() == Some(Kind::Ping as u8) {
            if let Some(reply) = plaza_wire::frame::answer_ping(&JsonCodec, &frame[1..], None) {
              let mut wire = Vec::new();
              delimit(&reply, &mut wire);
              let _ = pong_writer.lock().await.write_all(&wire).await;
            }
            continue;
          }
          let ops = decode_ops(&frame);
          if !ops.is_empty() {
            sink.lock().extend(ops);
          }
        }
        match read_half.read(&mut chunk).await {
          Ok(0) | Err(_) => return,
          Ok(n) => framing.feed(&chunk[..n]),
        }
      }
    });

    Ok(Self { heard, writer, task })
  }

  pub fn refusal(&self) -> Option<Refusal> {
    self.heard.lock().iter().find_map(|op| match op {
      ArcadeOp::Refused { reason } => Some(*reason),
      _ => None,
    })
  }

  pub fn was_admitted(&self) -> bool {
    self
      .heard
      .lock()
      .iter()
      .any(|op| matches!(op, ArcadeOp::Admitted { .. }))
  }

  pub fn closure(&self) -> Option<String> {
    self.heard.lock().iter().find_map(|op| match op {
      ArcadeOp::Closed { reason } => Some(reason.clone()),
      _ => None,
    })
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

