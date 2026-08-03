//! Queue depths and limits derived from what an application does.
//!
//! [`Queues`] and [`Limits`] are the individual knobs. A [`Workload`] is the
//! handful of answers they can be computed from: how fast the server ticks, how
//! many players it holds, how long a client may stall, how big a frame gets.
//! Presets are named `Workload`s, so starting from one and changing a field is
//! the same mechanism as writing one from scratch.
//!
//! Every formula here is measured in `benches/saturation.rs` except where its
//! doc says otherwise.

use std::time::Duration;

use crate::manager::{Limits, Queues, DEFAULT_CONDITIONER_CAPACITY, DEFAULT_PROBE_SLOTS};

/// Bytes buffered below the outbound queue: socket buffers plus the transport's
/// own writer.
///
/// A client absorbs this much before plaza's queue is what fills, so it is
/// subtracted from a derived depth. Measured at roughly 540 KiB and constant
/// across an 80x range of frame sizes, which makes it a byte budget rather than
/// a frame count. It belongs to the host and its tuning, not to the
/// application: a deployment that has measured its own should set
/// [`Workload::socket_buffer_bytes`].
pub const DEFAULT_SOCKET_BUFFER_BYTES: usize = 540 * 1024;

/// Whether losing a message or waiting for one is the worse outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
  /// A dropped op is a defect: a bid, a card, a move.
  LossFree,
  /// A stale frame is worthless because the next one supersedes it.
  LatencyFirst,
}

/// Floor under a derived per-connection depth.
///
/// **Not measured, and deliberately not zero.** A derivation subtracts
/// [`Workload::socket_buffer_bytes`], which is a property of a host this crate
/// has not seen: get it too large and the arithmetic concludes the queue is
/// unnecessary. A server should be able to hold a tick of output whatever the
/// socket underneath turns out to do.
pub const MIN_OUTBOUND_CAPACITY: usize = 4;

/// Floor under a derived controller-facing depth.
///
/// **Not measured.** `ops_per_player_per_tick: 0` is a claim about intent, not
/// a guarantee: a client can always send, and a queue derived to depth one
/// refuses the second frame that arrives in a tick. These hold small items on
/// one server rather than bytes per connection, so the floor is cheap.
pub const MIN_CONTROLLER_CAPACITY: usize = 8;

impl Priority {
  /// Multiplier on a derived depth.
  ///
  /// **Not measured.** Under [`LossFree`](Self::LossFree) an under-provisioned
  /// queue costs correctness, and under [`LatencyFirst`](Self::LatencyFirst) it
  /// costs one frame that the next one would have replaced, so the two do not
  /// deserve the same margin.
  fn headroom(self) -> usize {
    match self {
      Priority::LossFree => 2,
      Priority::LatencyFirst => 1,
    }
  }
}

/// What an application does, in the terms its author already knows.
#[derive(Debug, Clone)]
pub struct Workload {
  /// How often the server broadcasts, in Hz.
  pub tick_rate: u32,
  /// Connections held at once.
  pub peak_players: usize,
  /// Op batches one player sends per tick. Zero for a read-only audience.
  pub ops_per_player_per_tick: u32,
  /// How long a client may stop reading without losing a frame.
  pub stall_tolerance: Duration,
  /// Connections that may arrive at once. A server restart reconnects
  /// everybody, which is why this defaults to [`peak_players`](Self::peak_players).
  pub join_burst: usize,
  /// What one tick of the application's own work costs. `None` reads as the
  /// whole tick period, since a tick that costs more than that cannot keep up.
  pub tick_budget: Option<Duration>,
  /// Largest frame this build sends, in bytes.
  pub max_payload: usize,
  /// Bytes this build will spend on outbound queues across every connection.
  /// `None` sizes from [`stall_tolerance`](Self::stall_tolerance) alone.
  pub memory_budget: Option<usize>,
  /// Whether a drop or a wait is the worse failure.
  pub priority: Priority,
  /// What the host buffers under the outbound queue. See
  /// [`DEFAULT_SOCKET_BUFFER_BYTES`].
  pub socket_buffer_bytes: usize,
}

impl Default for Workload {
  fn default() -> Self {
    Self {
      tick_rate: 20,
      peak_players: 32,
      ops_per_player_per_tick: 1,
      stall_tolerance: Duration::from_millis(500),
      join_burst: 32,
      tick_budget: None,
      max_payload: 4 * 1024,
      memory_budget: None,
      priority: Priority::LatencyFirst,
      socket_buffer_bytes: DEFAULT_SOCKET_BUFFER_BYTES,
    }
  }
}

/// How many events at `rate` fit into `over`.
fn events(rate: u32, over: Duration) -> usize {
  (rate as f64 * over.as_secs_f64()).ceil() as usize
}

impl Workload {
  /// Racing and shooters: fast, small frames, a stale one is worthless.
  pub fn action() -> Self {
    Self {
      tick_rate: 60,
      peak_players: 16,
      ops_per_player_per_tick: 2,
      stall_tolerance: Duration::from_millis(200),
      join_burst: 16,
      max_payload: 512,
      priority: Priority::LatencyFirst,
      ..Self::default()
    }
  }

  /// Many entities in one view: fast, and large enough per frame that memory
  /// rather than time is what bounds the queue.
  pub fn horde() -> Self {
    Self {
      tick_rate: 60,
      peak_players: 64,
      ops_per_player_per_tick: 2,
      stall_tolerance: Duration::from_secs(1),
      join_burst: 64,
      max_payload: 40 * 1024,
      memory_budget: Some(256 * 1024 * 1024),
      priority: Priority::LatencyFirst,
      ..Self::default()
    }
  }

  /// Cards, auctions, anything where a lost op is a lost move.
  pub fn turn_based() -> Self {
    Self {
      tick_rate: 4,
      peak_players: 8,
      ops_per_player_per_tick: 1,
      stall_tolerance: Duration::from_secs(5),
      join_burst: 8,
      max_payload: 8 * 1024,
      priority: Priority::LossFree,
      ..Self::default()
    }
  }

  /// Many long-lived connections carrying light traffic, bounded by what the
  /// per-connection queues cost multiplied by the connection count.
  pub fn social_relay() -> Self {
    Self {
      tick_rate: 10,
      peak_players: 4096,
      ops_per_player_per_tick: 1,
      stall_tolerance: Duration::from_secs(2),
      join_burst: 512,
      max_payload: 512,
      memory_budget: Some(512 * 1024 * 1024),
      priority: Priority::LatencyFirst,
      ..Self::default()
    }
  }

  /// An audience that receives and never sends.
  pub fn spectator() -> Self {
    Self {
      tick_rate: 30,
      peak_players: 4096,
      ops_per_player_per_tick: 0,
      stall_tolerance: Duration::from_secs(2),
      join_burst: 512,
      max_payload: 8 * 1024,
      memory_budget: Some(512 * 1024 * 1024),
      priority: Priority::LatencyFirst,
      ..Self::default()
    }
  }

  /// Presence-dominated: connections arrive, wait, and leave for a room.
  pub fn lobby() -> Self {
    Self {
      tick_rate: 1,
      peak_players: 2048,
      ops_per_player_per_tick: 0,
      stall_tolerance: Duration::from_secs(10),
      join_burst: 2048,
      max_payload: 4 * 1024,
      priority: Priority::LossFree,
      ..Self::default()
    }
  }

  /// What ships without a workload named at all.
  pub fn local() -> Self {
    Self::default()
  }

  /// One tick of the application's own work.
  fn tick_cost(&self) -> Duration {
    self
      .tick_budget
      .unwrap_or_else(|| Duration::from_secs_f64(1.0 / self.tick_rate.max(1) as f64))
  }

  /// Frames the host already buffers before the outbound queue is what fills.
  fn socket_frames(&self) -> usize {
    self.socket_buffer_bytes / self.max_payload.max(1)
  }
}

impl Queues {
  /// Depths for a workload.
  ///
  /// - `outbound` is what a stall needs beyond what the socket already holds,
  ///   then capped by [`Workload::memory_budget`].
  /// - `inbound` and `decoded` are one term: the bridge blocks moving frames
  ///   between them, so what a burst absorbs is their sum. It is split evenly
  ///   because only the sum was measurable.
  /// - `presence` is the join burst exactly; nothing buffers underneath it.
  /// - `conditioner` is left at its default: what it holds follows from the
  ///   [`LinkProfile`](crate::conditioner::LinkProfile) set at runtime, which a
  ///   workload does not describe.
  pub fn for_workload(workload: &Workload) -> Self {
    let headroom = workload.priority.headroom();

    let stall = events(workload.tick_rate, workload.stall_tolerance);
    let mut outbound = (stall.saturating_sub(workload.socket_frames()) * headroom).max(MIN_OUTBOUND_CAPACITY);
    if let Some(budget) = workload.memory_budget {
      // The budget outranks the floor: a build that cannot afford four frames
      // per connection needs to know that, not to be quietly given them.
      let per_connection = budget / workload.peak_players.max(1) / workload.max_payload.max(1);
      outbound = outbound.min(per_connection);
    }

    let arrivals_per_tick = workload.peak_players * workload.ops_per_player_per_tick as usize;
    let backlog = events(workload.tick_rate, workload.tick_cost()) * arrivals_per_tick;
    let pipe = (arrivals_per_tick + backlog) * headroom;

    Self {
      inbound: pipe.div_ceil(2).max(MIN_CONTROLLER_CAPACITY),
      decoded: pipe.div_ceil(2).max(MIN_CONTROLLER_CAPACITY),
      presence: (workload.join_burst * headroom).max(MIN_CONTROLLER_CAPACITY),
      outbound: outbound.max(1),
      conditioner: DEFAULT_CONDITIONER_CAPACITY,
    }
  }
}

impl Limits {
  /// Caps for a workload.
  ///
  /// Both byte caps take the largest frame the build sends, doubled, and never
  /// fall below 64 KiB: the cap exists to bound what one client can make the
  /// server allocate, and a build that sizes it to its own frames exactly will
  /// refuse the first one that grows.
  ///
  /// `probe_slots` is left at its default. It follows from the round trip and
  /// the probe schedule rather than from anything a workload says.
  pub fn for_workload(workload: &Workload) -> Self {
    let cap = workload.max_payload.saturating_mul(2).max(64 * 1024);
    Self {
      max_frame_bytes: cap,
      max_message_bytes: cap,
      probe_slots: DEFAULT_PROBE_SLOTS,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn presets() -> Vec<(&'static str, Workload)> {
    vec![
      ("action", Workload::action()),
      ("horde", Workload::horde()),
      ("turn_based", Workload::turn_based()),
      ("social_relay", Workload::social_relay()),
      ("spectator", Workload::spectator()),
      ("lobby", Workload::lobby()),
      ("local", Workload::local()),
    ]
  }

  #[test]
  fn every_preset_derives_something_a_channel_can_be_built_from() {
    for (name, workload) in presets() {
      let queues = Queues::for_workload(&workload);
      for (field, depth) in [
        ("inbound", queues.inbound),
        ("decoded", queues.decoded),
        ("presence", queues.presence),
        ("outbound", queues.outbound),
        ("conditioner", queues.conditioner),
      ] {
        assert!(depth > 0, "{name}.{field} is zero; session_channel panics on that");
      }
    }
  }

  #[test]
  fn a_preset_that_derives_what_another_does_is_a_second_name_not_a_second_preset() {
    let derived: Vec<_> = presets()
      .into_iter()
      .map(|(name, workload)| {
        let q = Queues::for_workload(&workload);
        let l = Limits::for_workload(&workload);
        (
          name,
          (q.inbound, q.decoded, q.presence, q.outbound, l.max_frame_bytes),
        )
      })
      .collect();

    let mut collisions = Vec::new();
    for (position, (name, shape)) in derived.iter().enumerate() {
      for (other, other_shape) in &derived[..position] {
        if shape == other_shape {
          collisions.push(format!("{name} derives what {other} does: {shape:?}"));
        }
      }
    }
    assert!(collisions.is_empty(), "{}", collisions.join("; "));
  }

  #[test]
  fn a_stall_the_socket_already_covers_needs_no_queue() {
    let small = Workload {
      tick_rate: 60,
      stall_tolerance: Duration::from_millis(200),
      max_payload: 512,
      ..Workload::default()
    };
    assert!(
      small.socket_frames() > events(small.tick_rate, small.stall_tolerance),
      "1049 frames of socket against 12 of stall"
    );
    assert_eq!(Queues::for_workload(&small).outbound, MIN_OUTBOUND_CAPACITY);
  }

  #[test]
  fn a_large_frame_leaves_the_queue_carrying_the_stall() {
    let large = Workload {
      tick_rate: 60,
      stall_tolerance: Duration::from_secs(1),
      max_payload: 40 * 1024,
      memory_budget: None,
      priority: Priority::LatencyFirst,
      ..Workload::default()
    };
    assert_eq!(large.socket_frames(), 13);
    assert_eq!(Queues::for_workload(&large).outbound, 47);
  }

  #[test]
  fn a_memory_budget_caps_what_a_stall_asks_for() {
    let mut hungry = Workload::horde();
    hungry.memory_budget = None;
    let uncapped = Queues::for_workload(&hungry).outbound;

    hungry.memory_budget = Some(16 * 1024 * 1024);
    let capped = Queues::for_workload(&hungry).outbound;

    assert!(capped < uncapped, "{capped} should be under {uncapped}");
  }

  #[test]
  fn an_audience_that_never_sends_still_gets_a_usable_inbound_queue() {
    let queues = Queues::for_workload(&Workload::spectator());
    assert_eq!(queues.inbound, MIN_CONTROLLER_CAPACITY);
    assert_eq!(queues.decoded, MIN_CONTROLLER_CAPACITY);
  }
}
