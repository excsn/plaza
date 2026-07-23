//! One `step` that advances both peers one logical frame: sample each side's
//! input, cross them over the simulated wire (with a redundant tail), deliver
//! what is due, and advance each peer under the current policy.
//!
//! Everything here is host-native and headless, so the tests at the bottom are
//! where the rollback behaviour is actually pinned. The renderer only reads the
//! results of `step`.

use std::collections::VecDeque;

use plaza_client_utils::ack::AckWindow;
use plaza_client_utils::net_sim::{LatencyLink, Rng};
use plaza_client_utils::rollback::Frame;

use crate::sim::peer::Peer;
use crate::sim::types::{opponent_input, Controls, Input, InputPacket, Redundancy, FRAME_MS, OPPONENT, REDUNDANCY, YOU};

/// How many past inputs each side keeps. Longer than [`REDUNDANCY`], because a
/// targeted resend reaches back to whatever the peer's acknowledgement says is
/// missing, and that gap can be older than a blind tail ever repeats.
const HISTORY: usize = REDUNDANCY * 5;

pub struct World {
  /// Your peer: you are the local player, the opponent is predicted.
  peer_a: Peer,
  /// The opponent's peer: the mirror, where you are the predicted one.
  peer_b: Peer,

  /// The wire, one queue per direction. `to_a` carries the opponent's inputs to
  /// your peer; `to_b` carries yours to theirs.
  to_a: LatencyLink<InputPacket>,
  to_b: LatencyLink<InputPacket>,
  rng: Rng,

  wall_ms: u64,
  /// The last few inputs each side produced, for the redundant packet tail.
  a_hist: VecDeque<(Frame, Input)>,
  b_hist: VecDeque<(Frame, Input)>,

  /// What each side has actually received from the other. Each puts its own
  /// window on the wire so the other knows what to resend.
  a_heard: AckWindow,
  b_heard: AckWindow,
  /// The newest window each side has been *told* about, which is what it resends
  /// from. Always at least a latency out of date, and that staleness is the
  /// technique's real cost: a sender resends frames the peer may already have.
  a_told: AckWindow,
  b_told: AckWindow,

  bytes_sent: u64,
  inputs_sent: u64,
  packets_sent: u64,
}

impl World {
  pub fn new(seed: u64) -> Self {
    Self {
      peer_a: Peer::new(YOU),
      peer_b: Peer::new(OPPONENT),
      to_a: LatencyLink::new(),
      to_b: LatencyLink::new(),
      rng: Rng::new(seed),
      wall_ms: 0,
      a_hist: VecDeque::with_capacity(REDUNDANCY),
      b_hist: VecDeque::with_capacity(REDUNDANCY),
      a_heard: AckWindow::new(),
      b_heard: AckWindow::new(),
      a_told: AckWindow::new(),
      b_told: AckWindow::new(),
      bytes_sent: 0,
      inputs_sent: 0,
      packets_sent: 0,
    }
  }

  fn record(hist: &mut VecDeque<(Frame, Input)>, frame: Frame, input: Input) {
    match hist.back_mut() {
      // While a delay-based peer stalls, it re-samples the same frame: overwrite.
      Some(back) if back.0 == frame => back.1 = input,
      _ => {
        hist.push_back((frame, input));
        while hist.len() > HISTORY {
          hist.pop_front();
        }
      }
    }
  }

  /// A packet of this side's inputs, newest first.
  ///
  /// The three policies differ only in which past frames get repeated, which is
  /// the whole comparison: blind repeats a fixed tail whether or not anyone needs
  /// it, targeted asks the peer's acknowledgement and repeats exactly the gaps.
  fn build_packet(hist: &VecDeque<(Frame, Input)>, heard: &AckWindow, told: &AckWindow, mode: Redundancy) -> InputPacket {
    let newest = hist.back().copied();
    match mode {
      Redundancy::None => InputPacket {
        inputs: newest.into_iter().collect(),
        ack: None,
      },
      Redundancy::Blind => InputPacket {
        inputs: hist.iter().rev().take(REDUNDANCY).copied().collect(),
        ack: None,
      },
      Redundancy::Targeted => {
        let mut inputs: Vec<(Frame, Input)> = newest.into_iter().collect();
        if let Some(current) = newest.map(|(f, _)| f) {
          // Only the *gaps*: frames older than the peer's newest that it did not
          // get. Frames newer than that are still in flight and are not evidence
          // of anything, so resending them is the mistake that makes this policy
          // pointless. It costs one extra round trip to recover a real loss,
          // because the gap is only visible once a later packet has arrived, and
          // that delay is the price of not guessing.
          //
          // Capped so a peer that has gone quiet cannot grow the packet without
          // bound, and floored because past the rollback horizon a resend is
          // wasted bytes.
          let floor = current.saturating_sub(HISTORY as u64);
          for frame in told.missing_since(floor) {
            if inputs.len() >= REDUNDANCY {
              break;
            }
            if let Some(entry) = hist.iter().find(|(f, _)| *f == frame) {
              inputs.push(*entry);
            }
          }
        }
        InputPacket {
          inputs,
          ack: heard.encode(),
        }
      }
    }
  }

  /// Advances the whole picture by one logical frame, holding `your_input` for
  /// your box this frame.
  pub fn step(&mut self, your_input: Input, controls: &Controls) {
    self.wall_ms += FRAME_MS;

    // Each peer samples the input for the frame it is about to run. Yours comes
    // from the keyboard; the opponent's is the deterministic patrol.
    let a_frame = self.peer_a.current_frame();
    let b_frame = self.peer_b.current_frame();
    let opp = opponent_input(b_frame);

    self.peer_a.queue_local(your_input);
    self.peer_b.queue_local(opp);

    Self::record(&mut self.a_hist, a_frame, your_input);
    Self::record(&mut self.b_hist, b_frame, opp);

    // Cross the inputs over the wire: yours to their peer, theirs to yours.
    let yours = Self::build_packet(&self.a_hist, &self.a_heard, &self.a_told, controls.redundancy);
    let theirs = Self::build_packet(&self.b_hist, &self.b_heard, &self.b_told, controls.redundancy);
    for packet in [&yours, &theirs] {
      self.bytes_sent += packet.bytes() as u64;
      self.inputs_sent += packet.inputs.len() as u64;
      self.packets_sent += 1;
    }
    self.to_b.send(self.wall_ms, yours, controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut self.rng);
    self.to_a.send(self.wall_ms, theirs, controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut self.rng);

    for pkt in self.to_a.drain_due(self.wall_ms) {
      for (frame, _) in &pkt.inputs {
        self.a_heard.observe(*frame);
      }
      if let Some((newest, mask)) = pkt.ack {
        self.a_told = AckWindow::from_encoded(newest, mask);
      }
      self.peer_a.deliver(&pkt.inputs);
    }
    for pkt in self.to_b.drain_due(self.wall_ms) {
      for (frame, _) in &pkt.inputs {
        self.b_heard.observe(*frame);
      }
      if let Some((newest, mask)) = pkt.ack {
        self.b_told = AckWindow::from_encoded(newest, mask);
      }
      self.peer_b.deliver(&pkt.inputs);
    }

    // Advance each peer. In predict mode both step every frame; a delay-based peer
    // stalls here until its remote input has arrived.
    self.peer_a.advance(controls);
    self.peer_b.advance(controls);
  }

  pub fn peer_a(&self) -> &Peer {
    &self.peer_a
  }

  pub fn peer_b(&self) -> &Peer {
    &self.peer_b
  }

  /// The newest frame both peers have every player's input for.
  pub fn common_confirmed_frame(&self) -> Option<Frame> {
    Some(self.peer_a.remote_confirmed_frame()?.min(self.peer_b.remote_confirmed_frame()?))
  }

  /// Whether the two peers agree at their common confirmed frame: the determinism
  /// check, and the demo's headline. `None` until both have a confirmed frame.
  pub fn in_sync(&self) -> Option<bool> {
    let cf = self.common_confirmed_frame()?;
    Some(self.peer_a.state_at(cf)? == self.peer_b.state_at(cf)?)
  }

  pub fn packets_in_flight(&self) -> usize {
    self.to_a.in_flight() + self.to_b.in_flight()
  }

  /// Wire cost, both directions.
  pub fn bytes_per_sec(&self) -> f64 {
    if self.wall_ms == 0 {
      return 0.0;
    }
    self.bytes_sent as f64 / (self.wall_ms as f64 / 1000.0)
  }

  pub fn mean_inputs_per_packet(&self) -> f64 {
    if self.packets_sent == 0 {
      return 0.0;
    }
    self.inputs_sent as f64 / self.packets_sent as f64
  }

  /// Share of the frames each side sent that the other actually holds, over the
  /// acknowledgement window. The number a redundancy policy exists to keep high.
  pub fn delivery_rate(&self) -> f64 {
    // The window covers WINDOW slots behind the newest *plus* the newest itself,
    // so the denominator is WINDOW + 1. Dividing by WINDOW alone reports 102% on
    // a perfect link, which is the kind of readout that makes a real regression
    // look like rounding.
    let received = self.a_heard.received_in_window() + self.b_heard.received_in_window();
    received as f64 / (2.0 * (plaza_client_utils::ack::WINDOW + 1) as f64)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn idle() -> Input {
    Input { dx: 0, dy: 0 }
  }

  fn drive(world: &mut World, frames: usize, your: Input, c: &Controls) {
    for _ in 0..frames {
      world.step(your, c);
    }
  }

  #[test]
  fn with_rollback_the_peers_stay_in_sync_under_latency() {
    // You hold right; the opponent patrols and changes direction. Under real
    // latency both peers predict each other and both roll back, yet at every
    // confirmed frame they agree: that agreement is what rollback guarantees.
    let c = Controls {
      latency_ms: 120,
      predict: true,
      rollback: true,
      ..Controls::default()
    };
    let mut w = World::new(1);
    drive(&mut w, 400, Input { dx: 1, dy: 0 }, &c);

    // A rollback must actually have happened (the opponent turns), and finite.
    assert!(w.peer_a.rollback_count() > 0, "the opponent's turns forced rollbacks");
    assert_eq!(w.in_sync(), Some(true), "with rollback the peers agree at the confirmed frame");
    for b in w.peer_a.state().boxes {
      assert!(b.x.is_finite() && b.y.is_finite());
    }
  }

  #[test]
  fn without_rollback_the_peers_desync_under_latency() {
    // Same latency, prediction on, but rollback off: each peer trusts its guesses
    // forever, so their confirmed states drift apart.
    let c = Controls {
      latency_ms: 120,
      predict: true,
      rollback: false,
      ..Controls::default()
    };
    let mut w = World::new(1);
    drive(&mut w, 400, Input { dx: 1, dy: 0 }, &c);

    assert_eq!(w.peer_a.rollback_count(), 0, "rollback off never re-simulates");
    assert_eq!(w.in_sync(), Some(false), "trusted mispredictions leave the peers desynced");
  }

  #[test]
  fn delay_based_stays_in_sync_but_advances_slower_than_wall_time() {
    // Prediction off is delay-based lockstep: a peer waits for the remote input
    // before advancing, so it stays perfectly in sync but hitches under latency,
    // running fewer logical frames than wall frames.
    let c = Controls {
      latency_ms: 120,
      predict: false,
      ..Controls::default()
    };
    let mut w = World::new(1);
    let wall_frames = 400;
    drive(&mut w, wall_frames, Input { dx: 1, dy: 0 }, &c);

    assert_eq!(w.peer_a.rollback_count(), 0, "delay-based never predicts, so never rolls back");
    assert_eq!(w.in_sync(), Some(true), "waiting for inputs keeps it exactly in sync");
    assert!(
      (w.peer_a.current_frame() as usize) < wall_frames / 2,
      "under latency it advanced far fewer frames than wall time: {} of {}",
      w.peer_a.current_frame(),
      wall_frames
    );
  }

  #[test]
  fn redundancy_recovers_from_loss_and_keeps_the_peers_in_sync() {
    // A quarter of packets dropped. The redundant tail carries each input in
    // several packets, so a loss is backfilled and rollback still converges.
    let c = Controls {
      latency_ms: 100,
      loss_pct: 25.0,
      predict: true,
      rollback: true,
      redundancy: Redundancy::Blind,
      ..Controls::default()
    };
    let mut w = World::new(0xABCD);
    drive(&mut w, 500, Input { dx: 1, dy: 1 }, &c);

    assert_eq!(w.in_sync(), Some(true), "redundancy plus rollback survives loss in sync");
  }

  /// Runs, then settles the link and asks whether the peers agree.
  ///
  /// Asking on the last lossy frame does not answer the question: a peer holding
  /// a gap it has not been resent yet has simulated a predicted input there, so it
  /// legitimately differs for another round trip. The snapshot would report a
  /// desync that is really a recovery in progress.
  fn converges(c: &Controls, frames: usize, seed: u64) -> bool {
    let mut w = World::new(seed);
    drive(&mut w, frames, Input { dx: 1, dy: 1 }, c);
    let quiet = Controls { loss_pct: 0.0, ..*c };
    drive(&mut w, 90, Input { dx: 1, dy: 0 }, &quiet);
    w.in_sync() == Some(true)
  }

  #[test]
  fn targeted_redundancy_is_cheaper_on_a_clean_link() {
    // What the acknowledgement buys: on a link that is not dropping anything,
    // there are no gaps to fill, so every packet carries one input and the ack.
    // Blind repeats six regardless, because it has no way to find out.
    let clean = Controls {
      loss_pct: 0.0,
      latency_ms: 100,
      ..Controls::default()
    };
    let blind = {
      let mut w = World::new(0xACE0);
      drive(&mut w, 600, Input { dx: 1, dy: 1 }, &Controls { redundancy: Redundancy::Blind, ..clean });
      w
    };
    let targeted = {
      let mut w = World::new(0xACE0);
      drive(&mut w, 600, Input { dx: 1, dy: 1 }, &Controls { redundancy: Redundancy::Targeted, ..clean });
      w
    };
    assert!(
      targeted.bytes_per_sec() < blind.bytes_per_sec() * 0.8,
      "targeted should be clearly cheaper when there is nothing to resend: {:.0} against {:.0} B/s",
      targeted.bytes_per_sec(),
      blind.bytes_per_sec()
    );
    assert!(
      targeted.mean_inputs_per_packet() < 1.05,
      "and should be sending essentially one input per packet: {:.2}",
      targeted.mean_inputs_per_packet()
    );
  }

  #[test]
  fn blind_redundancy_is_cheaper_once_the_link_is_bad() {
    // The other half of the trade, and the reason this is a choice rather than an
    // upgrade. Ten bytes of acknowledgement per packet is a fixed toll, and once
    // enough is genuinely missing, targeted is resending most of the tail anyway
    // and paying the toll on top.
    let bad = Controls {
      loss_pct: 40.0,
      latency_ms: 100,
      ..Controls::default()
    };
    let mut blind = World::new(0xACE0);
    let mut targeted = World::new(0xACE0);
    drive(&mut blind, 800, Input { dx: 1, dy: 1 }, &Controls { redundancy: Redundancy::Blind, ..bad });
    drive(&mut targeted, 800, Input { dx: 1, dy: 1 }, &Controls { redundancy: Redundancy::Targeted, ..bad });
    assert!(
      targeted.bytes_per_sec() > blind.bytes_per_sec(),
      "the crossover is real: {:.0} targeted against {:.0} blind B/s",
      targeted.bytes_per_sec(),
      blind.bytes_per_sec()
    );
  }

  #[test]
  fn targeted_recovers_where_blind_eventually_gives_up() {
    // Measured, and it inverts the obvious reading of the bandwidth table. Blind
    // redundancy makes a *fixed number of attempts*: six packets carry each input
    // and then it is gone forever. Targeted keeps resending a frame until the
    // acknowledgement says it landed, so it makes as many attempts as the link
    // demands. At mild loss that difference is invisible; at 50% it is the whole
    // story, because 0.5^6 of the inputs outlive a blind tail.
    //
    // So the two policies are not "cheap and expensive". They are bounded effort
    // and bounded outcome, and the second degrades more gracefully.
    //
    // "More attempts" is not "unlimited attempts", which is the correction this
    // test earned: at 50% loss targeted converges every time and at 55% it drops
    // to 6 of 8. The real bound is not the attempt count, it is `HISTORY`. A gap
    // can only be resent while the input is still held, and once acknowledgements
    // are themselves being dropped, the round trip that reveals a gap can outlast
    // the window that could fix it. Lengthening the history moves the cliff; it
    // does not remove it.
    let brutal = Controls {
      loss_pct: 55.0,
      latency_ms: 100,
      ..Controls::default()
    };
    let blind_ok = (0..8).filter(|s| converges(&Controls { redundancy: Redundancy::Blind, ..brutal }, 1200, 0xACE0 + s)).count();
    let targeted_ok = (0..8).filter(|s| converges(&Controls { redundancy: Redundancy::Targeted, ..brutal }, 1200, 0xACE0 + s)).count();
    assert!(
      targeted_ok >= blind_ok,
      "retrying until acknowledged should never converge less often: {targeted_ok}/8 targeted against {blind_ok}/8 blind"
    );
  }

  #[test]
  fn the_delivery_readout_does_not_measure_success() {
    // Worth a test of its own, because the number is tempting and wrong. The
    // acknowledgement window counts how many of the last frames arrived, which
    // under blind redundancy is inflated by copies nobody needed. Targeted scores
    // *lower* on it while converging *more* often, so ranking policies by this
    // column would pick the worse one.
    let bad = Controls {
      loss_pct: 45.0,
      latency_ms: 100,
      ..Controls::default()
    };
    let mut blind = World::new(0xACE0);
    let mut targeted = World::new(0xACE0);
    drive(&mut blind, 800, Input { dx: 1, dy: 1 }, &Controls { redundancy: Redundancy::Blind, ..bad });
    drive(&mut targeted, 800, Input { dx: 1, dy: 1 }, &Controls { redundancy: Redundancy::Targeted, ..bad });
    assert!(blind.delivery_rate() > targeted.delivery_rate(), "the readout favours blind");
    assert!(blind.delivery_rate() <= 1.0 && targeted.delivery_rate() <= 1.0, "and is a share, so it cannot exceed 1");
  }

  #[test]
  fn a_zero_latency_link_needs_no_rollback() {
    // With instant delivery every input is confirmed the frame it is used, so
    // nothing is ever predicted wrong.
    let c = Controls {
      latency_ms: 0,
      predict: true,
      rollback: true,
      ..Controls::default()
    };
    let mut w = World::new(7);
    drive(&mut w, 200, Input { dx: 1, dy: 0 }, &c);

    assert_eq!(w.peer_a.rollback_count(), 0, "no latency, no misprediction");
    assert_eq!(w.in_sync(), Some(true));
  }

  #[test]
  fn the_prediction_horizon_tracks_latency() {
    let low = Controls {
      latency_ms: 48,
      ..Controls::default()
    };
    let high = Controls {
      latency_ms: 240,
      ..Controls::default()
    };
    let mut w_low = World::new(2);
    let mut w_high = World::new(2);
    drive(&mut w_low, 300, idle(), &low);
    drive(&mut w_high, 300, idle(), &high);

    assert!(
      w_high.peer_a.prediction_horizon() > w_low.peer_a.prediction_horizon(),
      "more latency means predicting further ahead: {} vs {}",
      w_high.peer_a.prediction_horizon(),
      w_low.peer_a.prediction_horizon()
    );
  }
}
