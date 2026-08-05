//! A client that speaks the wire by hand, so the door can be knocked on.

use std::sync::Arc;

use parking_lot::Mutex;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

use crate::types::{decode_ops, encode_ops, Account, ArcadeOp, Refusal};

pub struct Knock {
  pub heard: Arc<Mutex<Vec<ArcadeOp>>>,
  pub writer: Arc<Mutex<Option<tokio::io::WriteHalf<TcpStream>>>>,
  task: tokio::task::JoinHandle<()>,
}

impl Knock {
  /// Connects, says who it is, and collects whatever it is told.
  pub async fn arrive(addr: &str, account: Option<Account>) -> std::io::Result<Self> {
    let stream = TcpStream::connect(addr).await?;
    let heard = Arc::new(Mutex::new(Vec::new()));

    let (mut read_half, mut write_half) = tokio::io::split(stream);
    if let Some(account) = account {
      let frame = encode_ops(&[ArcadeOp::Hello { account }]);
      write_half.write_all(&(frame.len() as u32).to_be_bytes()).await?;
      write_half.write_all(&frame).await?;
    }

    let sink = heard.clone();
    let task = tokio::spawn(async move {
      loop {
        match read_split(&mut read_half).await {
          Ok(Some(frame)) => {
            let ops = decode_ops(&frame);
            if !ops.is_empty() {
              sink.lock().extend(ops);
            }
          }
          _ => return,
        }
      }
    });

    Ok(Self {
      heard,
      writer: Arc::new(Mutex::new(Some(write_half))),
      task,
    })
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
    let frame = encode_ops(ops);
    let mut guard = self.writer.lock();
    let Some(writer) = guard.as_mut() else {
      return Err(std::io::Error::other("no writer"));
    };
    writer.write_all(&(frame.len() as u32).to_be_bytes()).await?;
    writer.write_all(&frame).await
  }

  pub fn leave(self) {
    self.task.abort();
  }
}

async fn read_split(read: &mut tokio::io::ReadHalf<TcpStream>) -> std::io::Result<Option<Vec<u8>>> {
  use tokio::io::AsyncReadExt;
  let mut len = [0u8; 4];
  if read.read_exact(&mut len).await.is_err() {
    return Ok(None);
  }
  let len = u32::from_be_bytes(len) as usize;
  let mut body = vec![0u8; len];
  read.read_exact(&mut body).await?;
  Ok(Some(body))
}
