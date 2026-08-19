//! The numbers on screen: which phase owns the tick, and what the wire costs.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

/// Counters the send path writes and the panel reads.
#[derive(Default)]
pub struct WireStats {
  pub datagrams: AtomicU64,
  pub bytes: AtomicU64,
  pub dropped: AtomicU64,
  pub send_ns: AtomicU64,
  pub body: OnceLock<&'static str>,
}

impl WireStats {
  pub fn record(&self, bytes: usize, ns: u64) {
    self.datagrams.fetch_add(1, Ordering::Relaxed);
    self.bytes.fetch_add(bytes as u64, Ordering::Relaxed);
    self.send_ns.fetch_add(ns, Ordering::Relaxed);
  }
}

#[derive(Default, Clone, Copy)]
pub struct Phase {
  sum_ns: u64,
  worst_ns: u64,
  ticks: u64,
}

impl Phase {
  pub fn record(&mut self, ns: u64) {
    self.sum_ns += ns;
    self.worst_ns = self.worst_ns.max(ns);
    self.ticks += 1;
  }

  fn mean_us(&self) -> f64 {
    if self.ticks == 0 {
      return 0.0;
    }
    self.sum_ns as f64 / self.ticks as f64 / 1000.0
  }

  fn worst_us(&self) -> f64 {
    self.worst_ns as f64 / 1000.0
  }
}

/// Accumulates a second of ticks, prints one line, starts over.
pub struct Panel {
  pub step: Phase,
  pub rebuild: Phase,
  pub publish: Phase,
  pub assemble: Phase,
  elapsed_ms: f64,
  wire_datagrams: u64,
  wire_bytes: u64,
  wire_send_ns: u64,
  wire_dropped: u64,
}

impl Panel {
  pub fn new() -> Self {
    Self {
      step: Phase::default(),
      rebuild: Phase::default(),
      publish: Phase::default(),
      assemble: Phase::default(),
      elapsed_ms: 0.0,
      wire_datagrams: 0,
      wire_bytes: 0,
      wire_send_ns: 0,
      wire_dropped: 0,
    }
  }

  /// Feeds one tick's dt; prints and resets once a second has accumulated.
  pub fn tick(&mut self, dt_ms: f64, wire: &WireStats, ants: usize, watchers: usize, packed_cells: usize) {
    self.elapsed_ms += dt_ms;
    if self.elapsed_ms < 1000.0 {
      return;
    }

    let datagrams = wire.datagrams.load(Ordering::Relaxed);
    let bytes = wire.bytes.load(Ordering::Relaxed);
    let send_ns = wire.send_ns.load(Ordering::Relaxed);
    let dropped = wire.dropped.load(Ordering::Relaxed);
    let seconds = self.elapsed_ms / 1000.0;
    let pps = (datagrams - self.wire_datagrams) as f64 / seconds;
    let mbps = (bytes - self.wire_bytes) as f64 / seconds / 1.0e6;
    let send_ms = (send_ns - self.wire_send_ns) as f64 / 1.0e6 / seconds;
    let newly_dropped = dropped - self.wire_dropped;

    println!(
      "ants {ants} | watchers {watchers} | step {:.1}ms w{:.1} | rebuild {:.1}ms w{:.1} | publish {:.2}ms w{:.2} ({packed_cells} cells) | assemble {:.2}ms w{:.2} | {} {:.0} pkt/s {:.2} MB/s busy {:.1}ms/s{}",
      self.step.mean_us() / 1000.0,
      self.step.worst_us() / 1000.0,
      self.rebuild.mean_us() / 1000.0,
      self.rebuild.worst_us() / 1000.0,
      self.publish.mean_us() / 1000.0,
      self.publish.worst_us() / 1000.0,
      self.assemble.mean_us() / 1000.0,
      self.assemble.worst_us() / 1000.0,
      wire.body.get().copied().unwrap_or("udp"),
      pps,
      mbps,
      send_ms,
      if newly_dropped > 0 {
        format!(" dropped {newly_dropped}")
      } else {
        String::new()
      },
    );

    self.step = Phase::default();
    self.rebuild = Phase::default();
    self.publish = Phase::default();
    self.assemble = Phase::default();
    self.elapsed_ms = 0.0;
    self.wire_datagrams = datagrams;
    self.wire_bytes = bytes;
    self.wire_send_ns = send_ns;
    self.wire_dropped = dropped;
  }
}

impl Default for Panel {
  fn default() -> Self {
    Self::new()
  }
}
