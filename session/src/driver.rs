//! The connection loop every transport writes, minus the socket.
//!
//! [`Conditioner`](crate::conditioner::Conditioner),
//! [`ProbeState`](crate::control::ProbeState) and
//! [`LinkHandle`](crate::manager::LinkHandle) are the parts, and each is useful
//! alone. This is what plaza's own adapters assemble them into: both directions
//! of impairment, the probe schedule, and the deadline arithmetic that decides
//! when either of them wants attention.
//!
//! **It is a convenience, not a ceiling.** Nothing here reaches for anything a
//! transport outside this crate cannot reach for, which is the property that
//! makes it worth having rather than privileged: an adapter that needs
//! different behaviour uses the parts and writes its own, and loses nothing by
//! doing so. A transport whose link genuinely reorders is the case to expect,
//! since the conditioner below releases monotonically on the assumption that a
//! byte stream does not.
//!
//! The socket stays yours. So does framing, and so does enforcing
//! [`Limits::max_frame_bytes`](crate::manager::Limits::max_frame_bytes): those
//! are what a transport *is*.
//!
//! ```rust,ignore
//! let mut driver = LinkDriver::new(&manager, conn_id, codec.clone());
//!
//! loop {
//!   tokio::select! {
//!     inbound = socket.read_frame() => match driver.inbound(inbound?, Instant::now()) {
//!       Inbound::Reply(frame) => socket.write(frame).await?,
//!       Inbound::Forward(frame) => manager.forward_incoming(agent.clone(), frame).await,
//!       Inbound::Consumed => {}
//!     },
//!     outbound = to_client_rx.recv() => {
//!       if let Some(frame) = driver.outbound(outbound?, Instant::now()) {
//!         socket.write(frame).await?;
//!       }
//!     }
//!     _ = sleep_until(driver.deadline().unwrap_or_else(far_future)), if driver.deadline().is_some() => {
//!       for frame in driver.due(Instant::now()) {
//!         socket.write(frame).await?;
//!       }
//!     }
//!   }
//! }
//! ```

use std::sync::Arc;

use plaza::agent::AgentId;
use plaza::session::ConnectionId;
use tokio::time::Instant;

use crate::codec::WireCodec;
use crate::conditioner::Conditioner;
use crate::control::{self, Inbound, ProbeState, DOWN_SEED_FLIP};
use crate::manager::{ConnectionManager, Frame, LinkHandle, SessionClock};

/// One connection's link plane: impairment both ways, probes, and their
/// deadlines.
///
/// Holds a [`LinkHandle`] rather than reading the profile off the registry, so
/// the question every frame asks costs one relaxed load rather than the
/// registry's lock. That difference is measured in `docs/benches/passthrough.md`.
pub struct LinkDriver<ID: AgentId, C: WireCodec> {
  manager: Arc<ConnectionManager<ID>>,
  conn_id: ConnectionId,
  codec: C,
  clock: Option<SessionClock>,
  link: Arc<LinkHandle>,
  up: Conditioner,
  down: Conditioner,
  probe: ProbeState,
  next_probe: Option<Instant>,
  pending_inbound: Vec<Frame>,
  ejected: bool,
}

impl<ID: AgentId, C: WireCodec> LinkDriver<ID, C> {
  /// Builds the driver for a connection the manager has already registered.
  ///
  /// Returns `None` if `conn_id` is not registered, which is a transport that
  /// called this before `register` or after `deregister`.
  pub fn new(manager: &Arc<ConnectionManager<ID>>, conn_id: ConnectionId, codec: C) -> Option<Self> {
    let link = manager.link_handle(conn_id)?;
    let queues = manager.queues();
    let probe = ProbeState::new(manager.probes());
    Some(Self {
      manager: Arc::clone(manager),
      conn_id,
      codec,
      clock: manager.clock().cloned(),
      link,
      up: Conditioner::new(conn_id, queues.conditioner),
      down: Conditioner::new(conn_id ^ DOWN_SEED_FLIP, queues.conditioner),
      next_probe: probe.first_due(Instant::now()),
      probe,
      pending_inbound: Vec::new(),
      ejected: false,
    })
  }

  /// What to do with a frame that arrived from the peer.
  ///
  /// Answers probes, times the ones this side sent, and applies upstream
  /// impairment. What comes back is the transport's to act on: write a
  /// [`Reply`](Inbound::Reply) to the socket it was read from, hand a
  /// [`Forward`](Inbound::Forward) to
  /// [`forward_incoming`](ConnectionManager::forward_incoming).
  pub fn inbound(&mut self, frame: Frame, now: Instant) -> Inbound {
    if !self.link.impaired() && self.up.is_empty() {
      return self.route(frame);
    }
    let profile = self.link.read().up;
    if !self.up.push(frame, &profile, now) {
      self.manager.record_link_drop(self.conn_id);
    }
    Inbound::Consumed
  }

  /// A frame the controller queued, or `None` when the link is holding it.
  ///
  /// A held frame comes back from [`due`](Self::due) when its release time
  /// arrives.
  pub fn outbound(&mut self, frame: Frame, now: Instant) -> Option<Frame> {
    if !self.link.impaired() && self.down.is_empty() {
      return Some(frame);
    }
    let profile = self.link.read().down;
    if !self.down.push(frame, &profile, now) {
      self.manager.record_link_drop(self.conn_id);
    }
    None
  }

  /// When this connection next wants attention, or `None` if it does not.
  ///
  /// The earliest of a probe coming due and a held frame coming ready. A
  /// transport parks its timer arm on this.
  pub fn deadline(&self) -> Option<Instant> {
    control::earliest(
      self.next_probe,
      control::earliest(self.up.next_release(), self.down.next_release()),
    )
  }

  /// Everything owed to the socket now: released frames, and a probe if one is
  /// due.
  ///
  /// Inbound frames released by the upstream queue are forwarded on the way
  /// through, so what comes back is only what the transport writes.
  pub fn due(&mut self, now: Instant) -> Vec<Frame> {
    let mut owed = Vec::new();

    while let Some(frame) = self.down.pop_ready(now) {
      owed.push(frame);
    }

    while let Some(frame) = self.up.pop_ready(now) {
      match self.route(frame) {
        Inbound::Reply(reply) => owed.push(reply),
        Inbound::Forward(frame) => self.pending_inbound.push(frame),
        Inbound::Consumed | Inbound::Shed => {}
        Inbound::Eject => self.ejected = true,
      }
    }

    if self.next_probe.is_some_and(|at| at <= now) {
      owed.push(control::make_probe(&self.codec, &mut self.probe, now));
      self.next_probe = self.probe.interval().map(|gap| now + gap);
    }

    owed
  }

  /// Whether a frame released by the upstream queue exceeded a rate that ends
  /// connections, and this one should be closed.
  ///
  /// A held frame is judged when the link releases it rather than when it
  /// arrived, so this is what [`due`](Self::due) has no way to return: it hands
  /// back what the socket is owed, and a close is not a frame. The direct path
  /// needs no flag, since [`inbound`](Self::inbound) returns
  /// [`Eject`](Inbound::Eject) to its caller.
  pub fn ejected(&self) -> bool {
    self.ejected
  }

  /// Frames that came out of the upstream queue and belong to the application.
  ///
  /// Separate from [`due`](Self::due)'s return because these go to
  /// [`forward_incoming`](ConnectionManager::forward_incoming) rather than to
  /// the socket, and that is `async` while this is not.
  pub fn take_forwarded(&mut self) -> Vec<Frame> {
    std::mem::take(&mut self.pending_inbound)
  }

  fn route(&mut self, frame: Frame) -> Inbound {
    control::handle_inbound(
      frame,
      &self.codec,
      self.clock.as_ref(),
      &mut self.probe,
      self.conn_id,
      &self.manager,
    )
  }
}
