//! Naming an entity across a wire when its storage slot gets reused.
//!
//! A server that keeps entities in a dense array and recycles the gaps has the
//! cheapest possible identifier: the array index. It is also, on its own, wrong,
//! and wrong in a way that is very hard to see.
//!
//! Slot 41 dies and slot 41 is refilled on the same tick. Every message about
//! the old occupant that is still in flight, or that the client has not applied
//! yet, now names the new one. The client dutifully moves the wrong entity, or
//! deletes an entity that is alive, and neither side has any way to notice: the
//! index is valid, the message is well formed, and the mirror simply drifts.
//!
//! A generation counter fixes it. Bump it whenever a slot is refilled, and a
//! message naming `(41, generation 7)` is provably about the occupant the sender
//! meant, because the current occupant is generation 8 and the mismatch is
//! visible at the point of use.
//!
//! # Why this type exists rather than the convention
//!
//! Both sides have to encode the pair the same way, or their digests disagree
//! about a world they actually hold identically, and the recovery machinery
//! fires forever chasing a mismatch that is only in the arithmetic. That is a
//! genuinely miserable afternoon, and it is entirely preventable by having one
//! definition rather than two agreeing comments.
//!
//! It lives in the client crate because the server crate depends on this one:
//! `plaza_server_utils` re-exports it beside `DeltaBaseline`, whose keys are
//! exactly these.

/// A storage slot and the generation of its current occupant.
///
/// [`encode`](Self::encode)s to a `u64` as `(index << 16) | generation`, which
/// is the key space `DeltaBaseline` and `SetDigest` work in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SlotKey {
  /// Which slot. Dense and reused, so it is not an identity by itself.
  pub index: u32,
  /// How many times this slot has been refilled. What makes the pair an
  /// identity.
  pub generation: u16,
}

impl SlotKey {
  pub const fn new(index: u32, generation: u16) -> Self {
    Self { index, generation }
  }

  /// Packs to the `u64` that goes on the wire and into a digest.
  ///
  /// The index is shifted by 16 to leave the generation room, so an index past
  /// `2^48` would collide. Nothing this is for comes close, and an entity array
  /// that large has other problems first.
  pub const fn encode(self) -> u64 {
    ((self.index as u64) << 16) | self.generation as u64
  }

  pub const fn decode(key: u64) -> Self {
    Self {
      index: (key >> 16) as u32,
      generation: (key & 0xFFFF) as u16,
    }
  }

  /// The same slot with its generation dropped.
  ///
  /// For running deliberately without generations, which is worth being able to
  /// do: it is how you demonstrate what they are for. Every reference to a slot
  /// then matches whatever is in it, which is precisely the bug, made visible on
  /// demand instead of discovered in production.
  pub const fn ungenerational(self) -> Self {
    Self { index: self.index, generation: 0 }
  }

  /// Whether `other` names the same slot *and* the same occupant.
  pub const fn same_occupant(self, other: Self) -> bool {
    self.index == other.index && self.generation == other.generation
  }
}

impl From<SlotKey> for u64 {
  fn from(key: SlotKey) -> u64 {
    key.encode()
  }
}

impl From<u64> for SlotKey {
  fn from(key: u64) -> SlotKey {
    SlotKey::decode(key)
  }
}

/// The order freed slots are handed out again in.
///
/// **Part of the public contract rather than an implementation detail**, because
/// it decides how *clustered* recycled indices are, and that decides which wire
/// encoding is cheapest for a despawn set. Measured on a real many-entity
/// example: under [`Lifo`](ReusePolicy::Lifo) a burst of 233 despawns was 204
/// separate runs, a mean run length of **1.14**, which is why run-length
/// encoding lost decisively to delta-varint there. An allocator handing back
/// longer contiguous stretches would move that answer.
///
/// Neither is more correct. Pick [`Lifo`](ReusePolicy::Lifo) unless something
/// downstream cares about clustering, and if something does, measure rather than
/// assume: the encodings are close enough that the id allocation policy decides
/// the winner.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReusePolicy {
  /// Newest freed slot first. A stack, so the working set stays compact and
  /// cache-friendly, and recycled indices are scattered in whatever order things
  /// happened to die.
  #[default]
  Lifo,
  /// Oldest freed slot first. A queue, so a slot rests for as long as possible
  /// before being reused, which widens the window before a generation can wrap
  /// and tends to hand back indices in longer contiguous stretches.
  Fifo,
}

/// Hands out [`SlotKey`]s over a dense index space, recycling freed slots and
/// bumping a generation so stale handles stay detectable.
///
/// **It does not store your entities.** Keep them in a `Vec<T>` indexed by
/// [`SlotKey::index`], which is what the rest of these crates expect anyway:
/// `VisibilitySet` takes dense `u32` indices, and `DeltaBaseline` and
/// `DeltaMirror` key on [`SlotKey::encode`]. An allocator that owned the payload
/// would force an application to restructure around it and would still not
/// compose with those.
///
/// ```
/// # use plaza_client_utils::slot::SlotAllocator;
/// let mut pool = SlotAllocator::new();
/// let mut world: Vec<&str> = Vec::new();
///
/// let key = pool.alloc();
/// if world.len() <= key.index as usize {
///   world.resize(key.index as usize + 1, "");
/// }
/// world[key.index as usize] = "goblin";
///
/// assert!(pool.is_live(key));
/// assert!(pool.free(key));
/// assert!(!pool.is_live(key), "the handle no longer names anything");
///
/// // The index comes back, under a new occupant.
/// let reused = pool.alloc();
/// assert_eq!(reused.index, key.index);
/// assert_ne!(reused.generation, key.generation);
/// ```
///
/// # The ceiling, stated out loud
///
/// The generation is a `u16`, so a single slot freed 65,536 times wraps and a
/// handle from exactly that many reuses ago aliases the current occupant. That
/// is not hypothetical at every width: at a busy example's kill rate a `u8`
/// generation wraps a given slot in tens of minutes, where `u16` takes days.
/// Nothing can detect the wrap, so the mitigation is width and, if a session
/// runs long enough to matter, [`ReusePolicy::Fifo`] to spread reuse across the
/// whole index space instead of hammering the same slots.
#[derive(Clone, Debug, Default)]
pub struct SlotAllocator {
  /// Per index: the current generation, and whether it is occupied.
  slots: Vec<(u16, bool)>,
  free: std::collections::VecDeque<u32>,
  policy: ReusePolicy,
  live: usize,
}

impl SlotAllocator {
  pub fn new() -> Self {
    Self::default()
  }

  /// Pre-sizes the index space, so a known population allocates once.
  pub fn with_capacity(slots: usize) -> Self {
    Self {
      slots: Vec::with_capacity(slots),
      free: std::collections::VecDeque::with_capacity(slots),
      policy: ReusePolicy::default(),
      live: 0,
    }
  }

  /// Chooses the reuse order. See [`ReusePolicy`], which is a wire decision as
  /// much as a storage one.
  pub fn with_policy(mut self, policy: ReusePolicy) -> Self {
    self.policy = policy;
    self
  }

  /// Takes a slot, reusing a freed index when one is available.
  ///
  /// Indices are dense: the space only grows when nothing is free, so it settles
  /// at the high-water mark of simultaneously live entities rather than at the
  /// total ever created.
  pub fn alloc(&mut self) -> SlotKey {
    let index = match self.policy {
      ReusePolicy::Lifo => self.free.pop_back(),
      ReusePolicy::Fifo => self.free.pop_front(),
    };
    self.live += 1;
    match index {
      Some(index) => {
        let slot = &mut self.slots[index as usize];
        slot.1 = true;
        SlotKey::new(index, slot.0)
      }
      None => {
        let index = self.slots.len() as u32;
        self.slots.push((0, true));
        SlotKey::new(index, 0)
      }
    }
  }

  /// Releases a slot, if this key names its current occupant.
  ///
  /// Returns whether it actually freed anything: a key naming an occupant that
  /// is already gone is refused rather than freeing whoever moved in, which is
  /// the same check [`DeltaMirror::remove`] makes on the other side of the wire.
  ///
  /// The generation is bumped **here**, on free, rather than when the slot is
  /// next taken. That matters: an outstanding handle should stop naming anything
  /// the moment its subject dies, not whenever something happens to want the
  /// index. Between those two moments is exactly the window a delta stream is
  /// re-deriving retractions in.
  ///
  /// [`DeltaMirror::remove`]: crate::mirror::DeltaMirror::remove
  pub fn free(&mut self, key: SlotKey) -> bool {
    let Some(slot) = self.slots.get_mut(key.index as usize) else {
      return false;
    };
    if !slot.1 || slot.0 != key.generation {
      return false;
    }
    slot.1 = false;
    slot.0 = slot.0.wrapping_add(1);
    self.free.push_back(key.index);
    self.live -= 1;
    true
  }

  /// Whether this key names a slot's current occupant.
  pub fn is_live(&self, key: SlotKey) -> bool {
    self.slots.get(key.index as usize).is_some_and(|slot| slot.1 && slot.0 == key.generation)
  }

  /// Whether anybody occupies `index`.
  ///
  /// For a loop that walks the index space while mutating storage indexed by it,
  /// where [`iter`](Self::iter) would hold a borrow across the mutation. Cheaper
  /// than [`key`](Self::key) when the generation is not wanted.
  pub fn is_occupied(&self, index: u32) -> bool {
    self.slots.get(index as usize).is_some_and(|slot| slot.1)
  }

  /// The key naming whoever currently occupies `index`, if anybody does.
  ///
  /// For going the other way: application state is stored by index, so this is
  /// how a bare index becomes a handle that can go on the wire.
  pub fn key(&self, index: u32) -> Option<SlotKey> {
    self.slots.get(index as usize).filter(|slot| slot.1).map(|slot| SlotKey::new(index, slot.0))
  }

  /// Every live key, in index order.
  pub fn iter(&self) -> impl Iterator<Item = SlotKey> + '_ {
    self
      .slots
      .iter()
      .enumerate()
      .filter(|(_, slot)| slot.1)
      .map(|(index, slot)| SlotKey::new(index as u32, slot.0))
  }

  /// How many slots are occupied.
  pub fn len(&self) -> usize {
    self.live
  }

  pub fn is_empty(&self) -> bool {
    self.live == 0
  }

  /// How many indices exist, live or free.
  ///
  /// The width application storage must cover, and the id space a presence
  /// bitmask spans, so it is the number to size a `Vec<T>` or a `VisibilitySet`
  /// by rather than [`len`](Self::len).
  pub fn index_space(&self) -> usize {
    self.slots.len()
  }

  /// Frees every slot, bumping each live generation so outstanding handles are
  /// invalidated rather than silently matching a rebuilt world.
  ///
  /// Keeps the index space, so application storage indexed by it stays valid.
  pub fn clear(&mut self) {
    self.free.clear();
    for (index, slot) in self.slots.iter_mut().enumerate() {
      if slot.1 {
        slot.1 = false;
        slot.0 = slot.0.wrapping_add(1);
      }
      self.free.push_back(index as u32);
    }
    self.live = 0;
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_key_survives_the_round_trip() {
    // Both sides encode independently, so this is the property their digests
    // silently depend on.
    for (index, generation) in [(0u32, 0u16), (1, 1), (41, 7), (65_535, 65_535), (1_000_000, 3)] {
      let key = SlotKey::new(index, generation);
      assert_eq!(SlotKey::decode(key.encode()), key, "{index}:{generation}");
    }
  }

  #[test]
  fn a_reused_slot_is_a_different_key() {
    // The whole point. Without the generation these two are the same u64, and a
    // message about the first moves the second.
    let died = SlotKey::new(41, 7);
    let refilled = SlotKey::new(41, 8);
    assert_ne!(died.encode(), refilled.encode());
    assert!(!died.same_occupant(refilled));
    assert_eq!(died.index, refilled.index, "and it really is the same slot");
  }

  #[test]
  fn dropping_the_generation_makes_a_reused_slot_indistinguishable() {
    // The failure mode, on demand: this is what running without generations
    // costs, and being able to show it is why the mode exists.
    let died = SlotKey::new(41, 7).ungenerational();
    let refilled = SlotKey::new(41, 8).ungenerational();
    assert_eq!(died, refilled, "without generations the two occupants are one key");
  }

  #[test]
  fn keys_order_by_slot_then_occupant() {
    // Ordered iteration keeps a mirror's traversal stable, which matters when a
    // digest is folded over it.
    let mut keys = [SlotKey::new(2, 0), SlotKey::new(1, 9), SlotKey::new(1, 2)];
    keys.sort();
    assert_eq!(keys, [SlotKey::new(1, 2), SlotKey::new(1, 9), SlotKey::new(2, 0)]);
    assert!(keys.windows(2).all(|w| w[0].encode() < w[1].encode()), "the encoding sorts the same way");
  }
}

#[cfg(test)]
mod allocator_tests {
  use super::*;

  #[test]
  fn a_freed_index_comes_back_under_a_new_occupant() {
    let mut pool = SlotAllocator::new();
    let first = pool.alloc();
    assert!(pool.free(first));
    let second = pool.alloc();
    assert_eq!(second.index, first.index, "the index space stays dense");
    assert_ne!(second.generation, first.generation, "but the occupant is distinguishable");
    assert!(pool.is_live(second) && !pool.is_live(first));
  }

  #[test]
  fn a_stale_handle_cannot_free_whoever_took_the_slot() {
    // The failure the generation exists for. Without the check this releases a
    // live entity because a message about its predecessor arrived late.
    let mut pool = SlotAllocator::new();
    let dead = pool.alloc();
    pool.free(dead);
    let live = pool.alloc();

    assert!(!pool.free(dead), "a stale free must not fire");
    assert!(pool.is_live(live), "and the current occupant survived it");
    assert_eq!(pool.len(), 1);
  }

  #[test]
  fn freeing_twice_is_refused() {
    let mut pool = SlotAllocator::new();
    let key = pool.alloc();
    assert!(pool.free(key));
    assert!(!pool.free(key), "a double free must not put the index in the list twice");
    // If it had, these two would collide on the same index.
    let a = pool.alloc();
    let b = pool.alloc();
    assert_ne!(a.index, b.index);
  }

  #[test]
  fn the_generation_bumps_on_free_even_if_the_slot_is_never_reused() {
    // Bumping on free rather than on alloc is what makes a handle stop naming
    // anything the moment its subject dies, rather than whenever something
    // happens to want the index. That gap is exactly the window a delta stream
    // re-derives retractions in.
    let mut pool = SlotAllocator::new();
    let key = pool.alloc();
    pool.free(key);
    assert!(!pool.is_live(key), "invalidated immediately, with nothing reusing the slot");
    assert!(pool.key(key.index).is_none());
  }

  #[test]
  fn the_index_space_settles_at_the_high_water_mark() {
    // Dense indices are the whole point: they are what `VisibilitySet` and a
    // plain `Vec<T>` want, and the space must not grow with total entities ever
    // created.
    let mut pool = SlotAllocator::new();
    for _ in 0..100 {
      let batch: Vec<SlotKey> = (0..10).map(|_| pool.alloc()).collect();
      for key in batch {
        pool.free(key);
      }
    }
    assert_eq!(pool.index_space(), 10, "1000 allocations over 10 simultaneous slots");
    assert_eq!(pool.len(), 0);
  }

  #[test]
  fn the_reuse_policy_decides_the_order_indices_come_back() {
    // Not cosmetic: it decides how clustered a despawn set's ids are, and
    // therefore which wire encoding is cheapest for it.
    let mut lifo = SlotAllocator::new().with_policy(ReusePolicy::Lifo);
    let mut fifo = SlotAllocator::new().with_policy(ReusePolicy::Fifo);
    for pool in [&mut lifo, &mut fifo] {
      let keys: Vec<SlotKey> = (0..4).map(|_| pool.alloc()).collect();
      for key in keys {
        pool.free(key);
      }
    }
    assert_eq!(lifo.alloc().index, 3, "newest freed first");
    assert_eq!(fifo.alloc().index, 0, "oldest freed first, so a slot rests longest");
  }

  #[test]
  fn live_keys_are_enumerable_and_countable() {
    let mut pool = SlotAllocator::new();
    let keys: Vec<SlotKey> = (0..5).map(|_| pool.alloc()).collect();
    pool.free(keys[1]);
    pool.free(keys[3]);

    let live: Vec<SlotKey> = pool.iter().collect();
    assert_eq!(live, vec![keys[0], keys[2], keys[4]]);
    assert_eq!(pool.len(), 3);
    assert_eq!(pool.index_space(), 5, "the freed indices still exist, they are just empty");
    assert_eq!(pool.key(keys[2].index), Some(keys[2]));
  }

  #[test]
  fn clearing_invalidates_every_outstanding_handle() {
    // A world rebuilt under the players. Handles from the old one must not match
    // the new occupants, or a client's in-flight messages address the wrong
    // entities in a world it has not been told about yet.
    let mut pool = SlotAllocator::new();
    let old: Vec<SlotKey> = (0..3).map(|_| pool.alloc()).collect();
    pool.clear();
    assert_eq!(pool.len(), 0);
    assert!(old.iter().all(|key| !pool.is_live(*key)));

    let fresh = pool.alloc();
    assert!(!old.contains(&fresh), "a rebuilt world reissued an old handle");
    assert_eq!(pool.index_space(), 3, "and the index space is reused rather than regrown");
  }

  #[test]
  fn the_generation_wraps_and_the_ceiling_is_where_it_is_documented() {
    // Nothing can detect the wrap, so this pins where it happens rather than
    // pretending it does not. The mitigation is width, and `u16` is the width
    // `SlotKey` chose.
    let mut pool = SlotAllocator::new();
    let first = pool.alloc();
    assert_eq!(first.generation, 0);
    for _ in 0..u16::MAX as u32 + 1 {
      let key = pool.alloc_or_reuse_for_test();
      pool.free(key);
    }
    let wrapped = pool.alloc();
    assert_eq!(wrapped.index, 0, "still the same slot");
    assert_eq!(wrapped.generation, 0, "and its generation has come all the way round");
  }

  impl SlotAllocator {
    /// Takes the single slot this test cycles, whatever state it is in.
    fn alloc_or_reuse_for_test(&mut self) -> SlotKey {
      if let Some(key) = self.key(0) {
        return key;
      }
      self.alloc()
    }
  }
}
