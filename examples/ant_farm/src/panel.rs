//! The numbers on screen: which phase owns the tick, and what the wire costs.
//!
//! The panel accumulates a second of ticks and then produces one
//! [`StatsSnapshot`], which the server prints and also broadcasts, so an
//! observer window shows the server's own accounting rather than a model of
//! it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use crate::protocol::StatsSnapshot;

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

  fn mean_ms(&self) -> f32 {
    if self.ticks == 0 {
      return 0.0;
    }
    self.sum_ns as f32 / self.ticks as f32 / 1.0e6
  }

  fn worst_ms(&self) -> f32 {
    self.worst_ns as f32 / 1.0e6
  }
}

pub struct Panel {
  pub step: Phase,
  pub rebuild: Phase,
  pub publish: Phase,
  pub assemble: Phase,
  elapsed_ms: f64,
  wire_datagrams: u64,
  wire_bytes: u64,
  wire_send_ns: u64,
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
    }
  }

  /// Feeds one tick's dt; once a second has accumulated, returns the
  /// snapshot and starts over.
  pub fn tick(&mut self, dt_ms: f64, wire: &WireStats) -> Option<StatsSnapshot> {
    self.elapsed_ms += dt_ms;
    if self.elapsed_ms < 1000.0 {
      return None;
    }

    let datagrams = wire.datagrams.load(Ordering::Relaxed);
    let bytes = wire.bytes.load(Ordering::Relaxed);
    let send_ns = wire.send_ns.load(Ordering::Relaxed);
    let seconds = (self.elapsed_ms / 1000.0) as f32;

    let snapshot = StatsSnapshot {
      step_ms: self.step.mean_ms(),
      step_worst_ms: self.step.worst_ms(),
      rebuild_ms: self.rebuild.mean_ms(),
      rebuild_worst_ms: self.rebuild.worst_ms(),
      publish_ms: self.publish.mean_ms(),
      publish_worst_ms: self.publish.worst_ms(),
      assemble_ms: self.assemble.mean_ms(),
      assemble_worst_ms: self.assemble.worst_ms(),
      pps: (datagrams - self.wire_datagrams) as f32 / seconds,
      mbps: (bytes - self.wire_bytes) as f32 / seconds / 1.0e6,
      send_busy_ms: (send_ns - self.wire_send_ns) as f32 / 1.0e6 / seconds,
      dropped: wire.dropped.load(Ordering::Relaxed),
      body: wire.body.get().copied().unwrap_or("udp").to_string(),
      ..StatsSnapshot::default()
    };

    self.step = Phase::default();
    self.rebuild = Phase::default();
    self.publish = Phase::default();
    self.assemble = Phase::default();
    self.elapsed_ms = 0.0;
    self.wire_datagrams = datagrams;
    self.wire_bytes = bytes;
    self.wire_send_ns = send_ns;
    Some(snapshot)
  }
}

impl Default for Panel {
  fn default() -> Self {
    Self::new()
  }
}

/// The one-line form the server prints, which is also the headless panel.
pub fn print_line(s: &StatsSnapshot) {
  println!(
    "ants {} | watchers {} | step {:.1}ms w{:.1} | rebuild {:.1}ms w{:.1} | publish {:.2}ms w{:.2} ({} cells) | assemble {:.2}ms w{:.2} | tick {:.1}ms w{:.1} | {} {:.0} pkt/s {:.2} MB/s busy {:.1}ms/s{}",
    s.ants,
    s.watchers,
    s.step_ms,
    s.step_worst_ms,
    s.rebuild_ms,
    s.rebuild_worst_ms,
    s.publish_ms,
    s.publish_worst_ms,
    s.packed_cells,
    s.assemble_ms,
    s.assemble_worst_ms,
    s.tick_mean_ms,
    s.tick_worst_ms,
    s.body,
    s.pps,
    s.mbps,
    s.send_busy_ms,
    if s.dropped > 0 {
      format!(" dropped {}", s.dropped)
    } else {
      String::new()
    },
  );
}
