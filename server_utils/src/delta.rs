//! Per-subscriber bookkeeping for a set that is streamed as differences.
//!
//! [`relevance`](crate::relevance) answers *what* a subscriber should hold: run
//! the grid query, fill a [`VisibilitySet`](crate::relevance::VisibilitySet).
//! This answers the harder question that follows, which is what to actually
//! *send* given that the subscriber holds something already, that packets are
//! lost, and that either side may be wrong about what the other has.
//!
//! It is deliberately set-theoretic and knows nothing about what a key means.
//! Keys are `u64`, entering and leaving are the only events, and mapping a key
//! back to a spawn payload or a despawn reason is the application's job. What
//! lives here is the reliability, which is the part that is identical for every
//! game and that every game gets wrong in the same two ways.
//!
//! # The two failure modes this exists to prevent
//!
//! Both were shipped, in a real example, and both took days to find because the
//! symptom was far from the cause.
//!
//! **A subscriber that joins mid-session.** Servers usually track relevance for
//! every slot from startup, occupied or not. When a real client finally arrives,
//! that slot's baseline already describes most of the world, so the client's
//! first packet is a difference against a state it never received: it is sent
//! almost nothing and converges only as pieces of the world happen to become
//! newly relevant. Call [`reset`](DeltaBaseline::reset) when a subscriber takes
//! the slot and the first packet is a full baseline instead.
//!
//! **A mirror that diverges for any reason at all.** Once the server believes a
//! subscriber holds a key, that key is only ever sent as an update, and an
//! update for something you do not have is discarded. There is no path back, so
//! a single divergence is permanent no matter how much traffic follows. Carrying
//! the subscriber's own digest on its acknowledgement (see
//! [`observe_ack`](DeltaBaseline::observe_ack)) lets the server notice that the
//! two disagree and rebuild from nothing.
//!
//! # The digest is also the resume story
//!
//! The drift check has a second reading that matters as much as the first: it
//! is a **permission the client side builds on**. A client may discard any
//! stretch of the stream unread (a backgrounded tab's backlog, most commonly)
//! provided it also drops its mirror, because its next acknowledgement then
//! carries the digest of nothing and this type answers with a full baseline.
//! No resync-request message exists anywhere, and none is needed: dropping the
//! mirror is the request. The client half of that bargain is
//! `plaza_client_utils`' playout buffer and `plaza_ws`' backlog trim; the
//! server half is this type plus [`with_flow`](DeltaBaseline::with_flow),
//! which stops streaming full-rate full baselines to a subscriber that has
//! provably stopped reading.
//!
//! # Two invariants, both load bearing
//!
//! **The key must be the key the digest hashes.** If the application digests
//! `(index, generation)` pairs, that is what it must hand to this type, or the
//! drift check compares two unrelated numbers and either never fires or always
//! does. Encoding both into one `u64` is the usual answer.
//!
//! **Acknowledge states, not packets.** The frontier this walks is the newest
//! *contiguous* acknowledged sequence, not the newest bit set. Receiving packet
//! N+1 after losing N does not put a subscriber in the state N+1 implies,
//! because whatever N announced and N+1 had no reason to repeat is simply gone.
//! That walk is [`AckWindow::contiguous_base`], not something re-derived here:
//! it was re-derived here once, and wrongly, which is how the primitive came to
//! exist.
//!
//! [`AckWindow::contiguous_base`]: plaza_client_utils::ack::AckWindow::contiguous_base

use std::collections::{BTreeSet, VecDeque};

use plaza_client_utils::ack::AckWindow;

use crate::relevance::SetDigest;

/// How much of the reliability machinery to actually use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryPolicy {
  /// Diff against the last state *sent*. Simple, and wrong the moment a packet
  /// is lost: whatever it carried is never mentioned again, so the subscriber is
  /// permanently short of it while every readout looks healthy.
  ///
  /// Selectable because it is worth being able to demonstrate. A block that only
  /// knew how to be correct would make the failure it prevents invisible, and
  /// the failure is the whole reason the rest of this type exists.
  Naive,
  /// Diff against the newest state the subscriber has *acknowledged*, so a lost
  /// packet's contents are re-derived by the next difference rather than lost.
  /// Also enables the digest drift check and the stale-baseline rebuild.
  AckRecovery,
}

/// What to send this round.
#[derive(Clone, Debug, Default)]
pub struct DeltaPlan {
  /// The subscriber must clear its mirror before applying this, because what
  /// follows is the whole visible set rather than a difference from anything.
  ///
  /// Set when a subscriber is new, when its acknowledged baseline has aged out
  /// of history, or when its digest proved the mirror had drifted. Applying a
  /// full baseline onto an uncleared mirror would keep exactly the stale entries
  /// the rebuild exists to remove.
  pub full_baseline: bool,
  /// The sequence this plan's differences were computed against, or `None` for a
  /// difference from nothing. Worth putting on the wire: a subscriber that does
  /// not hold this baseline cannot apply the packet at all.
  pub baseline_seq: Option<u64>,
  /// Keys the subscriber does not hold and should.
  pub entered: Vec<u64>,
  /// Keys the subscriber may hold and should not.
  pub left: Vec<u64>,
}

/// The liveness half of flow control: when the subscriber last spoke, and when
/// it was last probed. Opt-in via [`DeltaBaseline::with_flow`]; time is in
/// whatever unit the application's clock uses.
#[derive(Clone, Debug)]
struct FlowControl {
  stalled_after: u64,
  keepalive_every: u64,
  /// When the subscriber last acknowledged, `None` until it first has (or after
  /// a [`reset`](DeltaBaseline::reset)): a fresh occupant gets its grace period
  /// measured from the first send decision, not from an epoch it never saw.
  last_ack: Option<u64>,
  last_keepalive: u64,
}

/// One subscriber's view of a streamed set: what it has been sent, what it has
/// acknowledged, and therefore what to send next.
///
/// One of these per subscriber. See the [module docs](self) for what it prevents
/// and the invariants it depends on.
///
/// ```ignore
/// // Once, when a subscriber takes the slot:
/// baseline.reset();
///
/// // Every send round:
/// if !baseline.should_send(now_ms) {
///   continue; // stalled, and not due a keepalive: send nothing
/// }
/// let plan = baseline.plan(&visible_keys, seq);
/// packet.full_baseline = plan.full_baseline;
/// packet.entered = plan.entered.iter().map(|k| spawn_payload(*k)).collect();
/// packet.left = plan.left.iter().map(|k| (handle(*k), reason(*k))).collect();
///
/// // When an acknowledgement comes back:
/// baseline.observe_ack_at(ack.newest, ack.mask, ack.digest, now_ms);
/// ```
#[derive(Clone, Debug)]
pub struct DeltaBaseline {
  /// What each recent packet would leave the subscriber holding, oldest first.
  sent: VecDeque<(u64, BTreeSet<u64>)>,
  /// The newest acknowledged state, once one is known.
  acked: Option<(u64, BTreeSet<u64>)>,
  /// Everything the subscriber might be holding: the acknowledged state plus
  /// everything announced since. What a retraction must be measured against.
  assumed_held: BTreeSet<u64>,
  /// The last state sent, which is what the naive policy diffs against.
  last_sent: BTreeSet<u64>,
  last_sent_seq: Option<u64>,
  history: usize,
  policy: RecoveryPolicy,
  needs_full: bool,
  full_rebuilds: u64,
  unacked: usize,
  flow: Option<FlowControl>,
}

impl DeltaBaseline {
  /// `history` is how many sent states to remember. Cover the most packets that
  /// can be in flight plus the acknowledgement's return trip; a state older than
  /// this cannot be recovered by re-derivation and forces a full rebuild.
  pub fn new(history: usize) -> Self {
    Self {
      sent: VecDeque::with_capacity(history.max(1)),
      acked: None,
      assumed_held: BTreeSet::new(),
      last_sent: BTreeSet::new(),
      last_sent_seq: None,
      history: history.max(1),
      policy: RecoveryPolicy::AckRecovery,
      // A subscriber that has been sent nothing holds nothing, and the honest
      // way to say that is a full baseline rather than a difference from a state
      // it was never in.
      needs_full: true,
      full_rebuilds: 0,
      unacked: 0,
      flow: None,
    }
  }

  /// Selects the reliability policy. See [`RecoveryPolicy`].
  pub fn with_policy(mut self, policy: RecoveryPolicy) -> Self {
    self.policy = policy;
    self
  }

  /// Enables flow control: a subscriber silent for `stalled_after` is throttled
  /// to one send every `keepalive_every`, both in the application's own clock
  /// units, until it acknowledges again.
  ///
  /// Why this belongs to the delta stream and not to the transport. Once a
  /// subscriber's acknowledged baseline ages out of history, **every** plan for
  /// it is a full baseline, so a reader that has stopped reading (a browser tab
  /// in the background: its socket keeps receiving while its frame loop does
  /// not run) is streamed the whole visible set at full rate, into a buffer it
  /// must pay for all at once on resume. Measured in the horde example that was
  /// tens of megabytes a minute, and a several-second freeze on refocus. The
  /// keepalive is what keeps the stream discoverable: the resumed client
  /// applies it, acknowledges it, and full rate resumes on the next round.
  ///
  /// Choosing `stalled_after`: match the client side's own discontinuity
  /// threshold (the point past which it restarts its timeline rather than
  /// playing through), and keep it several times the acknowledgement interval,
  /// so ordinary loss cannot trip it. A healthy subscriber acknowledges every
  /// applied packet, so silence at this scale means stopped, not unlucky.
  pub fn with_flow(mut self, stalled_after: u64, keepalive_every: u64) -> Self {
    self.flow = Some(FlowControl {
      stalled_after,
      keepalive_every: keepalive_every.max(1),
      last_ack: None,
      last_keepalive: 0,
    });
    self
  }

  /// Whether the subscriber has stopped acknowledging. Always `false` without
  /// [`with_flow`](Self::with_flow), and during a fresh subscriber's grace
  /// period (silence is measured from the first send decision, so a joiner is
  /// not born stalled).
  pub fn stalled(&self, now: u64) -> bool {
    let Some(flow) = &self.flow else {
      return false;
    };
    let Some(last_ack) = flow.last_ack else {
      return false;
    };
    now.saturating_sub(last_ack) > flow.stalled_after
  }

  /// Whether to build and send a packet to this subscriber this round.
  ///
  /// `true` for a live subscriber. For a stalled one, `true` once per
  /// `keepalive_every` and `false` otherwise, in which case skip the
  /// [`plan`](Self::plan) call entirely: not planning also leaves the sent
  /// history exactly where the last acknowledgement can still name it.
  pub fn should_send(&mut self, now: u64) -> bool {
    let Some(flow) = &mut self.flow else {
      return true;
    };
    // The grace period starts at the first decision, because construction and
    // reset have no clock: this is the first moment the stream knows the time.
    if flow.last_ack.is_none() {
      flow.last_ack = Some(now);
      return true;
    }
    if !self.stalled(now) {
      return true;
    }
    let flow = self.flow.as_mut().expect("checked above");
    if now.saturating_sub(flow.last_keepalive) >= flow.keepalive_every {
      flow.last_keepalive = now;
      return true;
    }
    false
  }

  /// Changes the policy on a live subscriber, forgetting any state the new
  /// policy cannot honour.
  pub fn set_policy(&mut self, policy: RecoveryPolicy) {
    if policy != self.policy {
      self.policy = policy;
      self.reset();
    }
  }

  /// Forgets everything and makes the next plan a full baseline.
  ///
  /// **Call this when a subscriber takes the slot.** Servers commonly track
  /// relevance for a slot whether or not anyone is in it, so by the time a real
  /// subscriber connects the slot's baseline already describes most of the
  /// world. Sending a difference against that leaves the joiner holding almost
  /// nothing, converging only as pieces of the world happen to become newly
  /// relevant, and it is invisible in every readout: the stream looks healthy
  /// and the subscriber is simply missing most of the world.
  ///
  /// A slot nobody has ever acknowledged is covered anyway, because an
  /// unacknowledged baseline is treated as unknown and sent in full. The case
  /// that still needs this call is a **reused** slot, where the previous
  /// occupant's acknowledged state is a perfectly plausible baseline for a
  /// subscriber that has never seen any of it.
  pub fn reset(&mut self) {
    self.sent.clear();
    self.acked = None;
    self.assumed_held.clear();
    self.last_sent.clear();
    self.last_sent_seq = None;
    self.needs_full = true;
    self.unacked = 0;
    // The new occupant's silence starts now, not where the old one's ended.
    if let Some(flow) = &mut self.flow {
      flow.last_ack = None;
      flow.last_keepalive = 0;
    }
  }

  /// Works out what to send, given the keys the subscriber should hold now.
  ///
  /// `seq` numbers this packet, and must be what the subscriber acknowledges.
  pub fn plan(&mut self, current: &BTreeSet<u64>, seq: u64) -> DeltaPlan {
    let recovering = self.policy == RecoveryPolicy::AckRecovery;
    // A baseline older than the history cannot be re-derived from anything still
    // known, so the only honest answer is to send the whole set. Not doing this
    // is a quiet failure: the frontier steps over the gap and the subscriber
    // stays short of that packet's contents forever.
    let stale = recovering && self.baseline_is_stale();
    // Under recovery, no acknowledged state means we genuinely do not know what
    // the subscriber holds, and a difference against what we last *sent* is
    // exactly the assumption this policy exists to avoid making. Anything lost
    // before the first acknowledgement would otherwise never be mentioned again,
    // which is the naive failure reappearing in the gap before recovery starts.
    // Full sets for the first round trip, then incremental for the rest of the
    // session.
    let unknown = recovering && self.acked.is_none();
    let rebuild = self.needs_full || stale;
    let full_baseline = rebuild || unknown;
    self.needs_full = false;

    if rebuild {
      self.acked = None;
      self.assumed_held.clear();
      // Forgetting this is how a rebuild becomes a no-op: with the acknowledged
      // baseline dropped the code falls back to the naive diff, and diffing
      // against a stale "what I last sent" emits nothing at all while the
      // rebuild counter still ticks.
      self.last_sent.clear();
      // A rebuild starts a new epoch, and the sent history is part of the old
      // one, twice over. A stale in-flight acknowledgement could name a
      // pre-rebuild state as the baseline for a subscriber that is about to
      // hold something else entirely. And after a subscriber restart, its
      // acknowledgement window has a gap at the stall boundary that
      // `contiguous_base` can never cross, so with the old history in place
      // the baseline stayed unknown, and unknown means a full set **every
      // round**, until the gap aged out: measured at ~25 consecutive full
      // baselines over 1.5 s. Cleared, the frontier restarts at the next
      // packet and one acknowledgement round trip ends the full sets.
      self.sent.clear();
      self.full_rebuilds += 1;
    }

    let mut plan = DeltaPlan {
      full_baseline,
      ..DeltaPlan::default()
    };

    match (&self.acked, recovering) {
      (Some((acked_seq, acked)), true) => {
        // Two baselines, built by opposite operations, because the two halves of
        // a difference answer two different questions.
        //
        // What to **send** must assume the least: the acknowledged state minus
        // anything a later packet may have retracted. Assuming more claims the
        // subscriber still holds something we told it to drop, so when that key
        // becomes relevant again it is never re-sent.
        //
        // What to **retract** must assume the most: everything it could be
        // holding. A key that entered and left inside the unacknowledged gap is
        // in neither the baseline nor the current set, so a single difference
        // never mentions it and the subscriber keeps it forever.
        let mut send_baseline = acked.clone();
        self.assumed_held.clone_from(acked);
        for (sent_seq, state) in &self.sent {
          if sent_seq > acked_seq {
            send_baseline.retain(|key| state.contains(key));
            self.assumed_held.extend(state.iter().copied());
          }
        }
        plan.baseline_seq = Some(*acked_seq);
        plan.entered = current.difference(&send_baseline).copied().collect();
        plan.left = self.assumed_held.difference(current).copied().collect();
      }
      _ if full_baseline => {
        // From nothing: the subscriber is clearing first, so everything visible
        // is new to it and there is nothing to retract.
        plan.baseline_seq = None;
        plan.entered = current.iter().copied().collect();
      }
      _ => {
        // The naive policy: difference against what was last sent, which assumes
        // every packet arrived. Wrong the moment one does not, and kept because
        // being able to demonstrate that is the point.
        plan.baseline_seq = self.last_sent_seq;
        plan.entered = current.difference(&self.last_sent).copied().collect();
        plan.left = self.last_sent.difference(current).copied().collect();
      }
    }

    // Remember what this packet leaves the subscriber holding, so a later
    // acknowledgement can name it as a baseline.
    self.sent.push_back((seq, current.clone()));
    while self.sent.len() > self.history {
      self.sent.pop_front();
    }
    self.last_sent.clone_from(current);
    self.last_sent_seq = Some(seq);
    self.unacked = self.sent.len();
    plan
  }

  /// Folds in an acknowledgement, moving the baseline forward and checking the
  /// subscriber's mirror against what we believe it reached.
  ///
  /// `digest` is the subscriber's own [`SetDigest`] over the keys it is actually
  /// holding. When it disagrees with the digest of the state we believe it
  /// reached, the mirror has drifted and no amount of further differences can
  /// repair it, so the next plan is a full rebuild. Pass the digest of an empty
  /// set if the application does not compute one, and the check is skipped.
  pub fn observe_ack(&mut self, newest: u64, mask: u64, digest: u64) {
    if self.policy != RecoveryPolicy::AckRecovery {
      return;
    }
    let window = AckWindow::from_encoded(newest, mask);
    // Contiguous, not newest-set: see the module docs. Stopping at the first gap
    // is the whole correctness of this, and it is `AckWindow`'s to get right
    // rather than something re-derived here. It was re-derived here once, and
    // wrongly, which is how the primitive came to exist.
    //
    // The first sequence not yet accounted for: one past the settled baseline, or
    // the oldest state still in history when nothing has been acknowledged yet.
    let first = self
      .acked
      .as_ref()
      .map(|(seq, _)| *seq + 1)
      .or_else(|| self.sent.front().map(|(seq, _)| *seq));
    if let Some(first) = first
      // `None` means the run is empty: either that packet never arrived, or the
      // subscriber has fallen so far behind that the window cannot speak about
      // it. Both mean the frontier does not move, and the staleness check in
      // `plan` is what rebuilds from the second case.
      && let Some(base) = window.contiguous_base(first)
      && let Some((seq, state)) = self.sent.iter().find(|(seq, _)| *seq == base)
    {
      self.acked = Some((*seq, state.clone()));
    }
    // The drift check, and it is deliberately not folded into the branch above:
    // a subscriber that re-acknowledges the same sequence still reports what it
    // is holding, and a mirror that lost something without losing a packet
    // reports it exactly then. Checking only when the frontier advances misses
    // precisely the case this exists to catch.
    //
    // Only compared when the frontier has reached the newest packet the
    // subscriber reports, which is to say when it has no gaps. With a gap its
    // digest describes packets beyond the frontier, so a disagreement would mean
    // "further ahead than the state we are comparing against" rather than
    // "wrong", and rebuilding on that would rebuild on ordinary packet loss.
    if let Some((acked_seq, acked)) = &self.acked
      && *acked_seq == newest
      && SetDigest::from_keys(acked.iter().copied()).digest() != digest
    {
      self.needs_full = true;
    }
    self.unacked = self.sent.iter().filter(|(seq, _)| !window.contains(*seq)).count();
  }

  /// [`observe_ack`](Self::observe_ack), and the acknowledgement's arrival time
  /// for flow control. Use this form whenever [`with_flow`](Self::with_flow) is
  /// on; the timestamp records under **either** policy, because liveness is a
  /// property of the subscriber, not of the recovery arithmetic.
  ///
  /// An ack from a subscriber currently stalled is **the resume signal**, and
  /// it starts a fresh epoch instead of being folded in. Its window spans the
  /// silence, and the keepalives inside the silence are sparse in the sequence
  /// space, so the contiguous walk pins the baseline at the first keepalive
  /// and every plan after resume diffs against a state as old as the stall,
  /// until staleness notices a second time: measured as ~25 consecutive full
  /// baselines over 1.5 s. Resetting instead makes the next plan one full
  /// baseline in a clean epoch, which the subscriber acknowledges contiguously,
  /// and the stream is deltas again after a single round trip.
  pub fn observe_ack_at(&mut self, newest: u64, mask: u64, digest: u64, now: u64) {
    let resuming = self.stalled(now);
    if let Some(flow) = &mut self.flow {
      flow.last_ack = Some(now);
    }
    if resuming {
      self.reset();
      if let Some(flow) = &mut self.flow {
        flow.last_ack = Some(now);
      }
      return;
    }
    self.observe_ack(newest, mask, digest);
  }

  /// Forces the next plan to be a full baseline. The application's own escape
  /// hatch, for a divergence it detected by some other means.
  pub fn request_full_baseline(&mut self) {
    self.needs_full = true;
  }

  /// How many times this subscriber has needed a full rebuild. The cost of
  /// recovery, and the number that says whether the history window is long
  /// enough for the loss and latency actually being seen.
  pub fn full_rebuilds(&self) -> u64 {
    self.full_rebuilds
  }

  /// Packets sent whose fate is still unknown.
  pub fn unacked(&self) -> usize {
    self.unacked
  }

  /// The newest sequence the subscriber has acknowledged reaching the state of.
  pub fn acked_seq(&self) -> Option<u64> {
    self.acked.as_ref().map(|(seq, _)| *seq)
  }

  fn baseline_is_stale(&self) -> bool {
    let Some((acked_seq, _)) = &self.acked else {
      return false;
    };
    self.sent.len() >= self.history && self.sent.front().is_some_and(|(oldest, _)| *acked_seq < *oldest)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn keys(items: &[u64]) -> BTreeSet<u64> {
    items.iter().copied().collect()
  }

  fn digest(items: &BTreeSet<u64>) -> u64 {
    SetDigest::from_keys(items.iter().copied()).digest()
  }

  /// A subscriber that applies plans faithfully, for driving the type the way a
  /// real client would.
  #[derive(Default)]
  struct Mirror {
    held: BTreeSet<u64>,
  }

  impl Mirror {
    fn apply(&mut self, plan: &DeltaPlan) {
      if plan.full_baseline {
        self.held.clear();
      }
      for key in &plan.left {
        self.held.remove(key);
      }
      for key in &plan.entered {
        self.held.insert(*key);
      }
    }
  }

  #[test]
  fn a_rebuild_starts_a_new_epoch_and_one_ack_round_trip_ends_the_full_sets() {
    // The resume churn. A restarted subscriber's ack window has a gap the
    // contiguous walk can never cross, so with the old sent history in place
    // the baseline stayed unknown, and unknown means a full set every round
    // until the gap aged out of history: ~25 consecutive full baselines.
    let mut b = DeltaBaseline::new(24);
    let world = keys(&[1, 2, 3]);
    let digest = SetDigest::from_keys(world.iter().copied()).digest();
    for seq in 1..=5 {
      b.plan(&world, seq);
      b.observe_ack(seq, u64::MAX, digest);
    }

    // The subscriber restarts and holds nothing; the server learns it.
    b.request_full_baseline();
    assert!(b.plan(&world, 6).full_baseline);

    // A stale in-flight ack from the old epoch must not become a baseline for
    // a subscriber that is about to hold something else entirely.
    b.observe_ack(5, u64::MAX, digest);
    assert_eq!(b.acked_seq(), None, "a pre-rebuild state is not a baseline for the new epoch");

    // It applies the full set, acknowledges it, and the churn is over.
    b.observe_ack(6, u64::MAX, digest);
    let settled = b.plan(&world, 7);
    assert!(!settled.full_baseline, "one acknowledged round trip ends the rebuild churn");
    assert!(settled.entered.is_empty(), "and the stream is deltas again");
  }

  #[test]
  fn a_silent_subscriber_is_throttled_to_keepalives_and_one_ack_restores_it() {
    // The hidden-tab pathology. Once the acknowledged baseline ages out of
    // history every plan is a full baseline, so without this a reader that has
    // stopped reading is streamed the whole visible set at full rate into a
    // buffer it pays for on resume.
    let mut b = DeltaBaseline::new(24).with_flow(3_000, 1_000);
    let world = keys(&[1, 2, 3]);
    let digest = SetDigest::from_keys(world.iter().copied()).digest();

    let mut now = 0u64;
    let mut seq = 0u64;
    let mut sent_while_stalled = 0;
    // Ten seconds of 62 ms rounds: acknowledged for the first second, silent after.
    for _ in 0..160 {
      now += 62;
      if b.should_send(now) {
        seq += 1;
        b.plan(&world, seq);
        if now <= 1_000 {
          b.observe_ack_at(seq, u64::MAX, digest, now);
        } else if now > 5_000 {
          sent_while_stalled += 1;
        }
      }
    }

    assert!(b.stalled(now), "three silent seconds is stalled");
    assert!(
      (3..=7).contains(&sent_while_stalled),
      "a stalled subscriber gets about one keepalive a second: {sent_while_stalled} in 5 s"
    );

    // One acknowledgement ends the throttle. It is also the resume signal, so
    // it opens a fresh epoch rather than being folded in: its window spans the
    // silence, and the keepalives inside the silence are sparse in sequence
    // space, so folding it in pins the baseline at the first keepalive and
    // every plan after resume is a full set until staleness fires again.
    b.observe_ack_at(seq, u64::MAX, digest, now);
    assert!(!b.stalled(now));
    assert!(b.should_send(now + 62), "an acknowledged subscriber is streamed to again");
    let fresh = b.plan(&world, seq + 1);
    assert!(fresh.full_baseline, "the resumed subscriber starts from one clean full set");
    b.observe_ack_at(seq + 1, u64::MAX, digest, now + 62);
    let settled = b.plan(&world, seq + 2);
    assert!(!settled.full_baseline, "and is back to deltas one round trip later");
    assert!(settled.entered.is_empty(), "with nothing spuriously re-sent");
  }

  #[test]
  fn without_flow_control_every_round_sends() {
    let mut b = DeltaBaseline::new(24);
    for round in 0..100u64 {
      assert!(b.should_send(round * 62), "flow control is opt-in");
    }
  }

  #[test]
  fn a_fresh_subscriber_is_not_born_stalled() {
    // Construction and reset have no clock, so silence is measured from the
    // first send decision: a joiner on a server whose clock reads an hour must
    // not start life throttled.
    let mut b = DeltaBaseline::new(24).with_flow(3_000, 1_000);
    assert!(b.should_send(3_600_000));
    assert!(!b.stalled(3_600_000));
    assert!(b.should_send(3_600_062), "full rate through the grace period");

    // The same grace applies to a reused slot.
    b.observe_ack_at(1, u64::MAX, 0, 3_600_062);
    b.reset();
    assert!(b.should_send(7_200_000), "the new occupant's silence starts now");
    assert!(!b.stalled(7_200_000));
  }

  #[test]
  fn a_fresh_subscriber_is_sent_the_whole_set_not_a_difference() {
    // The warm-join bug. The server has been tracking this slot for a while, so
    // its idea of "last sent" is most of the world by the time anyone connects.
    let mut b = DeltaBaseline::new(24);
    let world = keys(&[1, 2, 3, 4, 5]);
    for seq in 0..50 {
      b.plan(&world, seq);
    }

    // Now a real subscriber takes the slot.
    b.reset();
    let mut mirror = Mirror::default();
    let plan = b.plan(&world, 50);
    mirror.apply(&plan);

    assert!(plan.full_baseline, "a fresh subscriber must be told to start from nothing");
    assert_eq!(mirror.held, world, "and must receive the whole visible set at once");
  }

  #[test]
  fn a_reused_slot_without_a_reset_hands_the_new_occupant_the_old_ones_state() {
    // The failure that survives every other safeguard, kept as a test so it is
    // written down rather than remembered.
    //
    // While nobody has ever acknowledged, this type sends full sets anyway, so a
    // never-occupied slot is safe by accident. A slot whose *previous* occupant
    // acknowledged is not: that state looks like a perfectly good baseline, and
    // the new subscriber is sent the difference from a world it has never seen.
    let world = keys(&[1, 2, 3, 4, 5]);

    let mut b = DeltaBaseline::new(24);
    let mut previous = Mirror::default();
    previous.apply(&b.plan(&world, 0));
    b.observe_ack(0, 1, digest(&previous.held));

    // The previous occupant leaves and a new one takes the slot, with no reset.
    let mut joiner = Mirror::default();
    joiner.apply(&b.plan(&world, 1));
    assert!(joiner.held.is_empty(), "this is the bug: the joiner is sent nothing at all");

    // With the reset, which is the one line an application has to remember.
    b.reset();
    let mut joiner = Mirror::default();
    joiner.apply(&b.plan(&world, 2));
    assert_eq!(joiner.held, world, "a reset makes the next plan a full baseline");
  }

  #[test]
  fn a_lost_packet_is_re_derived_by_the_next_difference() {
    let mut b = DeltaBaseline::new(24);
    let mut mirror = Mirror::default();

    mirror.apply(&b.plan(&keys(&[1, 2]), 0));
    b.observe_ack(0, 1, digest(&mirror.held));

    // Packet 1 announces key 3 and is lost: the mirror never sees it.
    let _lost = b.plan(&keys(&[1, 2, 3]), 1);

    // Packet 2 is built against the acknowledged state, so it re-derives key 3.
    let plan = b.plan(&keys(&[1, 2, 3]), 2);
    mirror.apply(&plan);
    assert_eq!(mirror.held, keys(&[1, 2, 3]), "the lost key was re-sent, not lost forever");
  }

  #[test]
  fn the_naive_policy_loses_a_lost_packets_contents_forever() {
    // The teaching case, preserved deliberately. This is what the acknowledged
    // baseline exists to prevent, and it has to stay demonstrable.
    let mut b = DeltaBaseline::new(24).with_policy(RecoveryPolicy::Naive);
    let mut mirror = Mirror::default();
    mirror.apply(&b.plan(&keys(&[1, 2]), 0));

    let _lost = b.plan(&keys(&[1, 2, 3]), 1);
    let plan = b.plan(&keys(&[1, 2, 3]), 2);
    mirror.apply(&plan);

    assert!(!mirror.held.contains(&3), "the naive policy never mentions key 3 again");
  }

  #[test]
  fn a_drifted_mirror_is_caught_by_its_digest_and_rebuilt() {
    // The divergence that cannot heal itself: the server believes the subscriber
    // holds a key, so it is only ever sent as an update, and updates for things
    // you do not have are discarded.
    let mut b = DeltaBaseline::new(24);
    let mut mirror = Mirror::default();
    let world = keys(&[1, 2, 3, 4]);
    mirror.apply(&b.plan(&world, 0));
    b.observe_ack(0, 1, digest(&mirror.held));

    // Something eats a key. The cause does not matter; the point is that the
    // difference stream alone can never put it back.
    mirror.held.remove(&3);
    b.observe_ack(0, 1, digest(&mirror.held));

    let plan = b.plan(&world, 1);
    mirror.apply(&plan);
    assert!(plan.full_baseline, "the digest disagreement must force a rebuild");
    assert_eq!(mirror.held, world, "and the rebuild must restore the mirror exactly");
    assert_eq!(b.full_rebuilds(), 2, "the first baseline and this repair");
  }

  #[test]
  fn a_matching_digest_never_forces_a_rebuild() {
    let mut b = DeltaBaseline::new(24);
    let mut mirror = Mirror::default();
    let world = keys(&[1, 2, 3]);
    mirror.apply(&b.plan(&world, 0));
    b.observe_ack(0, 1, digest(&mirror.held));

    for seq in 1..30 {
      let plan = b.plan(&world, seq);
      mirror.apply(&plan);
      assert!(!plan.full_baseline, "a healthy stream never rebuilds (seq {seq})");
      b.observe_ack(seq, u64::MAX, digest(&mirror.held));
    }
  }

  #[test]
  fn a_key_that_enters_and_leaves_inside_the_gap_is_still_retracted() {
    // The union half of the two baselines. Without it, a key announced and then
    // dropped while acknowledgements were in flight is in neither the baseline
    // nor the current set, so nothing ever mentions it and the subscriber keeps
    // it forever.
    let mut b = DeltaBaseline::new(24);
    let mut mirror = Mirror::default();
    mirror.apply(&b.plan(&keys(&[1]), 0));
    b.observe_ack(0, 1, digest(&mirror.held));

    // Key 9 appears, is delivered, then goes away again, all before the next ack.
    mirror.apply(&b.plan(&keys(&[1, 9]), 1));
    let plan = b.plan(&keys(&[1]), 2);
    mirror.apply(&plan);

    assert_eq!(mirror.held, keys(&[1]), "the transient key was retracted");
  }

  #[test]
  fn a_baseline_older_than_the_history_forces_a_rebuild() {
    let mut b = DeltaBaseline::new(4);
    let mut mirror = Mirror::default();
    let world = keys(&[1, 2, 3]);
    mirror.apply(&b.plan(&world, 0));
    b.observe_ack(0, 1, digest(&mirror.held));

    // Silence from the subscriber for longer than the history holds.
    let mut rebuilt = false;
    for seq in 1..12 {
      let plan = b.plan(&world, seq);
      mirror.apply(&plan);
      rebuilt |= plan.full_baseline;
    }
    assert!(rebuilt, "an un-re-derivable baseline has to be rebuilt, not stepped over");
    assert_eq!(mirror.held, world, "and the subscriber ends up correct");
  }

  #[test]
  fn acknowledging_a_packet_after_a_gap_does_not_move_the_baseline_past_it() {
    // Receiving N+1 after losing N does not put a subscriber in the state N+1
    // implies. Taking the newest set bit instead of the contiguous frontier is
    // the mistake that makes recovery indistinguishable from no recovery.
    let mut b = DeltaBaseline::new(24);
    let mut mirror = Mirror::default();
    mirror.apply(&b.plan(&keys(&[1]), 0));
    b.observe_ack(0, 1, digest(&mirror.held));

    let _lost = b.plan(&keys(&[1, 2]), 1); // announces key 2, lost
    mirror.apply(&b.plan(&keys(&[1, 2]), 2)); // re-derived, arrives

    // The subscriber acknowledges 2 but not 1.
    let window = {
      let mut w = AckWindow::new();
      w.observe(0);
      w.observe(2);
      w
    };
    let (newest, mask) = window.encode().expect("a window with two packets encodes");
    b.observe_ack(newest, mask, digest(&mirror.held));
    assert_eq!(b.acked_seq(), Some(0), "the frontier stops at the gap");
  }

  #[test]
  fn a_long_lossy_run_still_converges() {
    // The property that matters in aggregate: whatever the loss pattern, a
    // subscriber that keeps acknowledging ends up holding exactly the right set.
    let mut b = DeltaBaseline::new(16);
    let mut mirror = Mirror::default();
    let mut window = AckWindow::new();
    let mut rng: u64 = 0x1234_5678;

    for seq in 0..400u64 {
      // A world that churns: keys drift in and out on different cycles.
      let world: BTreeSet<u64> = (0..24u64).filter(|k| (seq / (k + 2)) % 3 != 0).collect();
      let plan = b.plan(&world, seq);

      // Deterministic pseudo-random loss, about one packet in four.
      rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
      let delivered = (rng >> 33) % 4 != 0;
      if delivered {
        mirror.apply(&plan);
        window.observe(seq);
        if let Some((newest, mask)) = window.encode() {
          b.observe_ack(newest, mask, digest(&mirror.held));
        }
      }

      if delivered && seq > 40 {
        assert_eq!(mirror.held, world, "mirror diverged at seq {seq}");
      }
    }
  }
}
