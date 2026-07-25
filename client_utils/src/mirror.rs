//! The client's copy of a set of entities the server is streaming to it, and the
//! bookkeeping that says whether the copy is right.
//!
//! This is the other half of `plaza_server_utils::DeltaBaseline`. The server
//! decides what to send by diffing against what it believes a client holds; this
//! is what the client actually holds, and the two have to agree exactly or the
//! stream silently rots. Every serious bug found while building the examples
//! this was drawn from lived in that agreement rather than in either half.
//!
//! # Apply every packet, whatever baseline it names
//!
//! Worth stating first, because the instinct is the opposite, and the instinct
//! is what a strict delta protocol requires: if you cannot reach the baseline a
//! packet was built against, discard it.
//!
//! That is right when deltas are *relative* ("add three", "rotate by ten"). It
//! is wrong here, because these deltas carry absolute values: an entry carries
//! the entity in full, a removal names it outright, and a sample is a position
//! rather than an offset. Applying them is idempotent, and applying a superset
//! is harmless.
//!
//! Discarding instead starves the mirror, and measurably. An earlier version of
//! the example this came from did exactly that, and at 25% packet loss the
//! mirror emptied out while every agreement check still read perfect, because
//! the checks only ran over what had been applied.
//!
//! # What it counts, and why each number is separate
//!
//! - [`frames_lost`](DeltaMirror::frames_lost): gaps in the sequence. The
//!   *cause*: the wire dropped something.
//! - [`stale_refs`](DeltaMirror::stale_refs): messages naming an occupant this
//!   mirror no longer holds. Caught, rather than applied to whoever moved in.
//! - [`divergences`](DeltaMirror::divergences): times the digest disagreed. The
//!   *symptom*, and the only one that catches a drift no counter predicts.
//!
//! Keeping them apart is what makes a report diagnostic instead of decorative.
//! "Forty mismatches and zero frames lost" and "forty mismatches and forty
//! frames lost" are different bugs, and the first one is the interesting one.

use std::collections::BTreeMap;

use crate::ack::AckWindow;
use crate::digest::SetDigest;
use crate::slot::SlotKey;

/// What one entity's slot holds, plus the generation the mirror believes it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Held<Entity> {
  generation: u16,
  entity: Entity,
}

/// Whether the mirror matches what the server says it should be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agreement {
  /// The mirror holds exactly the set the server expects.
  Agreed,
  /// It does not. Carrying both digests so a report can say so, though neither
  /// number says *how*: for that, see [`DeltaMirror::divergence_from`].
  Diverged { held: u64, expected: u64 },
}

impl Agreement {
  pub fn agreed(self) -> bool {
    matches!(self, Agreement::Agreed)
  }
}

/// Which way a mirror diverged, given the server's own key set.
///
/// A digest detects a divergence and cannot diagnose one, so anything shipping a
/// digest wants a mode that ships the ground truth beside it. Which side the
/// difference falls on names the bug: `missing` means something was lost or
/// never sent, `extra` means a removal never landed or was rejected.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Divergence {
  /// Occupants this mirror holds that the server does not.
  pub extra: Vec<SlotKey>,
  /// Occupants the server expects that this mirror does not hold.
  pub missing: Vec<SlotKey>,
}

/// The client's mirror of a streamed entity set.
///
/// Generic over the entity so an application keeps whatever it needs per entity
/// (interpolation history, a smoother, a render kind); the mirror owns only the
/// keying, the agreement and the counters.
#[derive(Clone, Debug)]
pub struct DeltaMirror<Entity> {
  held: BTreeMap<u32, Held<Entity>>,
  acks: AckWindow,
  applied_seq: Option<u64>,
  generational: bool,
  digest: u64,
  frames_lost: u64,
  stale_refs: u64,
  divergences: u64,
}

impl<Entity> Default for DeltaMirror<Entity> {
  fn default() -> Self {
    Self::new()
  }
}

impl<Entity> DeltaMirror<Entity> {
  pub fn new() -> Self {
    Self {
      held: BTreeMap::new(),
      acks: AckWindow::new(),
      applied_seq: None,
      generational: true,
      digest: 0,
      frames_lost: 0,
      stale_refs: 0,
      divergences: 0,
    }
  }

  /// Turns generation checking off, so every reference to a slot matches
  /// whatever is in it.
  ///
  /// This is the broken mode, kept because being able to demonstrate the bug is
  /// worth more than pretending it cannot happen. With it off, a reference to a
  /// dead occupant is applied to its replacement and nothing is counted, which is
  /// exactly what makes recycled-slot corruption so hard to find in the wild.
  pub fn with_generations(mut self, generational: bool) -> Self {
    self.generational = generational;
    self
  }

  /// Switches generation checking at runtime, for an application that exposes it
  /// as a toggle.
  ///
  /// Changing it changes the key space, so the mirror is cleared: entries filed
  /// under the old scheme would be unreachable under the new one and would show
  /// up as a permanent divergence. A rebuild is one packet, and a mirror keyed
  /// two ways at once is forever.
  pub fn set_generations(&mut self, generational: bool) {
    if self.generational != generational {
      self.generational = generational;
      self.held.clear();
    }
  }

  /// Opens a packet: notes what the wire lost, acknowledges the sequence, and
  /// clears the mirror if this packet is a full baseline.
  ///
  /// Call once per packet, before applying anything in it. A full baseline is
  /// the server's repair for a mirror it can no longer reach by deltas, so the
  /// old contents must go rather than be merged with: merging is what leaves the
  /// drift that prompted the rebuild.
  pub fn begin(&mut self, seq: u64, full_baseline: bool) {
    // Every frame is numbered and the link is ordered, so a jump of more than one
    // means the wire lost what was in between. This is the direct measure, and it
    // is what separates "the network dropped it" from "we corrupted it".
    if let Some(previous) = self.applied_seq
      && seq > previous + 1
    {
      self.frames_lost += seq - previous - 1;
    }
    if self.applied_seq.is_none_or(|previous| seq > previous) {
      self.applied_seq = Some(seq);
    }
    self.acks.observe(seq);
    if full_baseline {
      self.held.clear();
    }
  }

  /// Files an entity under a key, replacing whatever was in the slot.
  ///
  /// A fresh occupant of a reused slot is exactly this: the previous tenant is
  /// gone, and the generation recorded is the new one.
  pub fn insert(&mut self, key: SlotKey, entity: Entity) {
    let key = self.normalise(key);
    self.held.insert(key.index, Held { generation: key.generation, entity });
  }

  /// Removes an occupant, if this key names the one actually held.
  ///
  /// A generation mismatch is counted as a stale reference and nothing is
  /// removed, which is the entire point: without the check this deletes a live
  /// entity that merely inherited the slot.
  pub fn remove(&mut self, key: SlotKey) -> Option<Entity> {
    let key = self.normalise(key);
    match self.held.get(&key.index) {
      Some(held) if held.generation != key.generation => {
        self.stale_refs += 1;
        None
      }
      Some(_) => self.held.remove(&key.index).map(|held| held.entity),
      None => None,
    }
  }

  /// The occupant this key names, or `None` if the mirror does not hold it.
  pub fn get(&self, key: SlotKey) -> Option<&Entity> {
    let key = self.normalise(key);
    self.held.get(&key.index).filter(|held| held.generation == key.generation).map(|held| &held.entity)
  }

  /// The occupant this key names, mutably, counting a generation mismatch as a
  /// stale reference.
  ///
  /// Use this for applying a sample. Returning `None` rather than the current
  /// occupant is what keeps a position meant for a dead entity off a live one.
  pub fn get_mut(&mut self, key: SlotKey) -> Option<&mut Entity> {
    let key = self.normalise(key);
    match self.held.get_mut(&key.index) {
      Some(held) if held.generation == key.generation => Some(&mut held.entity),
      Some(_) => {
        self.stale_refs += 1;
        None
      }
      None => None,
    }
  }

  pub fn contains(&self, key: SlotKey) -> bool {
    self.get(key).is_some()
  }

  /// Closes a packet: recomputes the digest and compares it to the server's.
  ///
  /// Everything in the packet has been applied by now, so the mirror must match
  /// what the server said it should be. This is the check a lost or malformed
  /// removal cannot hide from, because it is over the whole set rather than over
  /// the messages that happened to arrive.
  ///
  /// The digest it computes is kept, and [`digest`](Self::digest) should be sent
  /// on the next acknowledgement: the server compares it against the state it
  /// believes this client reached, which is the only way it can detect a drift
  /// its own delta stream can never repair.
  pub fn settle(&mut self, expected: u64) -> Agreement {
    self.digest = self.compute_digest();
    if self.digest == expected {
      Agreement::Agreed
    } else {
      self.divergences += 1;
      Agreement::Diverged { held: self.digest, expected }
    }
  }

  /// How this mirror differs from the server's own key set.
  ///
  /// For the debugging mode that ships the truth beside the digest. Cheap enough
  /// to call on a mismatch, far too expensive to send every packet, which is why
  /// it takes the keys rather than assuming they are always there.
  pub fn divergence_from<I: IntoIterator<Item = u64>>(&self, server_keys: I) -> Divergence {
    let theirs: std::collections::BTreeSet<u64> = server_keys.into_iter().collect();
    let mine: std::collections::BTreeSet<u64> = self.keys().map(SlotKey::encode).collect();
    Divergence {
      extra: mine.difference(&theirs).map(|k| SlotKey::decode(*k)).collect(),
      missing: theirs.difference(&mine).map(|k| SlotKey::decode(*k)).collect(),
    }
  }

  /// The digest of everything held, as of the last [`settle`](Self::settle).
  pub fn digest(&self) -> u64 {
    self.digest
  }

  /// The digest of everything held right now.
  ///
  /// [`SetDigest`] rather than a fold written out here, because the server folds
  /// the same keys with the same code. Two implementations that agree today are
  /// a disagreement waiting to happen, and it would present as a divergence in
  /// the world rather than in the arithmetic.
  fn compute_digest(&self) -> u64 {
    SetDigest::from_keys(self.keys().map(SlotKey::encode)).digest()
  }

  /// Which packets have arrived, to be sent back so the server's recovery has
  /// something to diff against.
  pub fn acks(&self) -> &AckWindow {
    &self.acks
  }

  /// Every key held, in slot order.
  pub fn keys(&self) -> impl Iterator<Item = SlotKey> + '_ {
    self.held.iter().map(|(index, held)| SlotKey::new(*index, held.generation))
  }

  /// Every occupant, in slot order.
  pub fn iter(&self) -> impl Iterator<Item = (SlotKey, &Entity)> + '_ {
    self.held.iter().map(|(index, held)| (SlotKey::new(*index, held.generation), &held.entity))
  }

  /// Every occupant, mutably, in slot order.
  pub fn iter_mut(&mut self) -> impl Iterator<Item = (SlotKey, &mut Entity)> + '_ {
    self.held.iter_mut().map(|(index, held)| (SlotKey::new(*index, held.generation), &mut held.entity))
  }

  pub fn values(&self) -> impl Iterator<Item = &Entity> + '_ {
    self.held.values().map(|held| &held.entity)
  }

  pub fn values_mut(&mut self) -> impl Iterator<Item = &mut Entity> + '_ {
    self.held.values_mut().map(|held| &mut held.entity)
  }

  pub fn len(&self) -> usize {
    self.held.len()
  }

  pub fn is_empty(&self) -> bool {
    self.held.is_empty()
  }

  /// Throws the mirror away, for a world being rebuilt under it.
  pub fn clear(&mut self) {
    self.held.clear();
  }

  /// Frames the wire lost, counted as gaps in the sequence.
  pub fn frames_lost(&self) -> u64 {
    self.frames_lost
  }

  /// References to an occupant this mirror no longer holds, rejected rather than
  /// applied to whoever took the slot.
  pub fn stale_refs(&self) -> u64 {
    self.stale_refs
  }

  /// Packets after which the mirror disagreed with the server's digest.
  pub fn divergences(&self) -> u64 {
    self.divergences
  }

  /// The newest sequence applied.
  pub fn applied_seq(&self) -> Option<u64> {
    self.applied_seq
  }

  fn normalise(&self, key: SlotKey) -> SlotKey {
    if self.generational { key } else { key.ungenerational() }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn mirror() -> DeltaMirror<&'static str> {
    DeltaMirror::new()
  }

  #[test]
  fn what_the_server_sent_is_what_the_mirror_holds() {
    let mut m = mirror();
    m.begin(1, true);
    m.insert(SlotKey::new(1, 0), "a");
    m.insert(SlotKey::new(2, 0), "b");
    assert_eq!(m.len(), 2);
    assert_eq!(m.get(SlotKey::new(1, 0)), Some(&"a"));

    m.begin(2, false);
    assert_eq!(m.remove(SlotKey::new(1, 0)), Some("a"));
    assert_eq!(m.len(), 1);
    assert!(!m.contains(SlotKey::new(1, 0)));
  }

  #[test]
  fn a_reference_to_a_dead_occupant_is_rejected_not_applied_to_its_replacement() {
    // The recycled-slot bug. A removal for the old tenant arrives after the slot
    // has been refilled; without the generation check it deletes a live entity.
    let mut m = mirror();
    m.begin(1, true);
    m.insert(SlotKey::new(41, 7), "old");
    m.insert(SlotKey::new(41, 8), "new");
    assert_eq!(m.len(), 1, "the slot holds one occupant at a time");

    assert_eq!(m.remove(SlotKey::new(41, 7)), None, "a stale removal must not fire");
    assert_eq!(m.stale_refs(), 1, "and it must be counted, not silently ignored");
    assert_eq!(m.get(SlotKey::new(41, 8)), Some(&"new"), "the live occupant survived");

    // A sample for the dead one is refused the same way.
    assert!(m.get_mut(SlotKey::new(41, 7)).is_none());
    assert_eq!(m.stale_refs(), 2);
  }

  #[test]
  fn without_generations_the_same_reference_corrupts_the_mirror() {
    // The failure this exists to prevent, demonstrated on purpose. Nothing is
    // counted, because from the mirror's point of view nothing went wrong.
    let mut m = mirror().with_generations(false);
    m.begin(1, true);
    m.insert(SlotKey::new(41, 8), "new");
    assert_eq!(m.remove(SlotKey::new(41, 7)), Some("new"), "a stale removal deleted a live entity");
    assert_eq!(m.stale_refs(), 0, "and there was no way to notice");
  }

  #[test]
  fn a_gap_in_the_sequence_is_counted_as_a_lost_frame() {
    // The direct measure of whether the wire is dropping things, which is what
    // separates a network problem from a bookkeeping one.
    let mut m = mirror();
    m.begin(1, true);
    m.begin(2, false);
    assert_eq!(m.frames_lost(), 0);
    m.begin(5, false);
    assert_eq!(m.frames_lost(), 2, "3 and 4 never arrived");
  }

  #[test]
  fn a_full_baseline_replaces_the_mirror_rather_than_merging_into_it() {
    // The server sends one precisely because it can no longer reach this mirror
    // by deltas. Merging would keep the drift that prompted the rebuild.
    let mut m = mirror();
    m.begin(1, true);
    m.insert(SlotKey::new(1, 0), "stale");
    m.insert(SlotKey::new(9, 0), "drifted");

    m.begin(2, true);
    assert!(m.is_empty(), "a rebuild starts from nothing");
    m.insert(SlotKey::new(1, 0), "fresh");
    assert_eq!(m.len(), 1);
    assert_eq!(m.get(SlotKey::new(1, 0)), Some(&"fresh"));
  }

  #[test]
  fn the_digest_agrees_when_both_sides_hold_the_same_set() {
    // Order independent, because the two sides build the set in different orders
    // and must still agree.
    let mut a = mirror();
    let mut b = mirror();
    a.begin(1, true);
    b.begin(1, true);
    for key in [SlotKey::new(3, 1), SlotKey::new(1, 0), SlotKey::new(2, 5)] {
      a.insert(key, "x");
    }
    for key in [SlotKey::new(2, 5), SlotKey::new(3, 1), SlotKey::new(1, 0)] {
      b.insert(key, "x");
    }
    b.settle(0);
    assert_eq!(a.settle(b.digest()), Agreement::Agreed);
  }

  #[test]
  fn a_drifted_mirror_is_caught_by_the_digest_and_can_say_how() {
    // A digest detects and cannot diagnose, so the mode that ships the truth
    // beside it is what turns a mismatch into a bug report. Which side the
    // difference falls on names the cause.
    let mut m = mirror();
    m.begin(1, true);
    m.insert(SlotKey::new(1, 0), "a");
    m.insert(SlotKey::new(2, 0), "b");
    // The server also expects slot 3, and does not expect slot 2.
    let server: Vec<u64> = [SlotKey::new(1, 0), SlotKey::new(3, 0)].iter().map(|k| k.encode()).collect();

    let mut expected = DeltaMirror::<&str>::new();
    expected.begin(1, true);
    expected.insert(SlotKey::new(1, 0), "a");
    expected.insert(SlotKey::new(3, 0), "c");
    expected.settle(0);

    let verdict = m.settle(expected.digest());
    assert!(!verdict.agreed());
    assert_eq!(m.divergences(), 1);

    let divergence = m.divergence_from(server);
    assert_eq!(divergence.extra, vec![SlotKey::new(2, 0)], "held something the server does not");
    assert_eq!(divergence.missing, vec![SlotKey::new(3, 0)], "missing something the server expects");
  }

  #[test]
  fn applying_the_same_packet_twice_changes_nothing() {
    // Why a packet is applied whatever baseline it names: these deltas carry
    // absolute values, so they are idempotent, and a client that discards what it
    // cannot rebase starves its own mirror instead.
    let mut m = mirror();
    for seq in [1, 1] {
      m.begin(seq, false);
      m.insert(SlotKey::new(1, 0), "a");
      m.insert(SlotKey::new(2, 0), "b");
      m.remove(SlotKey::new(9, 0));
    }
    m.settle(0);
    let once = m.digest();

    let mut single = mirror();
    single.begin(1, false);
    single.insert(SlotKey::new(1, 0), "a");
    single.insert(SlotKey::new(2, 0), "b");
    single.settle(0);
    assert_eq!(once, single.digest(), "a duplicate packet left the mirror somewhere else");
  }

  #[test]
  fn acknowledgements_track_what_actually_arrived() {
    // The whole input to the server's recovery: which of its deltas this client
    // is provably holding.
    let mut m = mirror();
    for seq in [1, 2, 4] {
      m.begin(seq, false);
    }
    let (newest, _mask) = m.acks().encode().expect("something arrived");
    assert_eq!(newest, 4);
    assert!(m.acks().contains(2));
    assert!(!m.acks().contains(3), "the gap is visible to the server too");
  }
}
