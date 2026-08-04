//! A guest, and optionally a badly behaved one.

use std::sync::Arc;

use parking_lot::Mutex;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

use crate::transport::{decode_ops, encode_ops};
use crate::types::{Parting, PartyOp, Seat};

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
      let frame = encode_ops(&[PartyOp::Sit { seat }]);
      write_half.write_all(&(frame.len() as u32).to_be_bytes()).await?;
      write_half.write_all(&frame).await?;
    }

    let sink = heard.clone();
    let task = tokio::spawn(async move {
      use tokio::io::AsyncReadExt;
      loop {
        let mut len = [0u8; 4];
        if read_half.read_exact(&mut len).await.is_err() {
          return;
        }
        let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
        if read_half.read_exact(&mut body).await.is_err() {
          return;
        }
        let ops = decode_ops(&body);
        if !ops.is_empty() {
          sink.lock().extend(ops);
        }
      }
    });

    Ok(Self {
      heard,
      writer: Arc::new(tokio::sync::Mutex::new(write_half)),
      task,
    })
  }

  pub async fn say(&self, ops: &[PartyOp]) -> std::io::Result<()> {
    let frame = encode_ops(ops);
    let mut writer = self.writer.lock().await;
    writer.write_all(&(frame.len() as u32).to_be_bytes()).await?;
    writer.write_all(&frame).await
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
