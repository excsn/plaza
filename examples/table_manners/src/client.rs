//! A guest, and optionally a badly behaved one.
//!
//! Answers the session's probes, so a guest who says nothing still has a live,
//! measured link: exactly the case AFK must remove and the probe traffic must
//! not save.

use std::sync::Arc;

use parking_lot::Mutex;
use plaza_session::codec::JsonCodec;
use plaza_session::DEFAULT_MAX_FRAME_BYTES;
use plaza_wire::frame::Kind;
use plaza_wire::framing::{delimit, LengthDelimited};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::types::{decode_ops, encode_ops, Parting, PartyOp, Seat};

pub struct Guest {
  pub heard: Arc<Mutex<Vec<PartyOp>>>,
  writer: Arc<tokio::sync::Mutex<tokio::io::WriteHalf<TcpStream>>>,
  task: tokio::task::JoinHandle<()>,
}

impl Guest {
  pub async fn arrive(addr: &str, seat: Option<Seat>) -> std::io::Result<Self> {
    let stream = TcpStream::connect(addr).await?;
    let heard = Arc::new(Mutex::new(Vec::new()));
    let (mut read_half, mut write_half) = tokio::io::split(stream);

    if let Some(seat) = seat {
      let mut wire = Vec::new();
      delimit(&encode_ops(&[PartyOp::Sit { seat }]), &mut wire);
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

  pub async fn say(&self, ops: &[PartyOp]) -> std::io::Result<()> {
    let mut wire = Vec::new();
    delimit(&encode_ops(ops), &mut wire);
    self.writer.lock().await.write_all(&wire).await
  }

  pub fn farewell(&self) -> Option<Parting> {
    self.heard.lock().iter().find_map(|op| match op {
      PartyOp::Farewell { reason, .. } => Some(*reason),
      _ => None,
    })
  }

  pub fn was_seated(&self) -> bool {
    self.heard.lock().iter().any(|op| matches!(op, PartyOp::Seated { .. }))
  }

  pub fn table(&self) -> Option<crate::types::Table> {
    self.heard.lock().iter().rev().find_map(|op| match op {
      PartyOp::Snapshot(t) => Some((**t).clone()),
      _ => None,
    })
  }

  /// Cuts the socket without a farewell, which is what a netdrop looks like.
  pub fn yank(self) {
    self.task.abort();
  }

  pub fn leave(self) {
    self.task.abort();
  }
}
