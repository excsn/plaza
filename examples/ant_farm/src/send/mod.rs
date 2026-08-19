//! The send path behind the transport: where a datagram becomes a syscall,
//! or on Linux with `--features xdp`, a descriptor on a TX ring.

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use tokio::net::UdpSocket;

use crate::panel::WireStats;

pub mod frame;
#[cfg(all(target_os = "linux", feature = "xdp"))]
pub mod xsk;

/// One outbound datagram, fire and forget. Datagram semantics mean failure is
/// counted, never retried: the next tick resends fresher state anyway.
pub trait SendPath: Send + Sync {
  fn send(&self, to: SocketAddr, bytes: &[u8]);
  fn label(&self) -> &'static str;
}

pub struct UdpSend {
  socket: Arc<UdpSocket>,
  wire: Arc<WireStats>,
}

impl UdpSend {
  pub fn new(socket: Arc<UdpSocket>, wire: Arc<WireStats>) -> Self {
    Self { socket, wire }
  }
}

impl SendPath for UdpSend {
  fn send(&self, to: SocketAddr, bytes: &[u8]) {
    let begun = Instant::now();
    match self.socket.try_send_to(bytes, to) {
      Ok(_) => self.wire.record(bytes.len(), begun.elapsed().as_nanos() as u64),
      Err(_) => {
        self.wire.dropped.fetch_add(1, Ordering::Relaxed);
      }
    }
  }

  fn label(&self) -> &'static str {
    "udp"
  }
}
