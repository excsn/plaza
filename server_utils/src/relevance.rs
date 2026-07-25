//! Relevance: deciding what each client needs to see.
//!
//! A multiplayer world is bigger than one screen, and its players stand in
//! different places. Sending every entity to every client is `players x entities`
//! and does not scale, a horde game (thousands of short-lived enemies) makes that
//! obvious. So the server sends each client only what is *relevant* to it, and
//! streams the churn (what entered or left a client's view) rather than the whole
//! set each tick.
//!
//! This module is the mechanism for that, as building blocks, not a policy:
//!
//! - [`morton`]: Z-order curve encode/decode, mapping 2D or 3D integer cells to a
//!   single integer that preserves spatial locality. The mathematical primitive
//!   underneath a grid; usable on its own for locality sorts and broadphase.
//! - [`GridQuantizer`]: continuous world coordinates to integer grid cells (and
//!   their Morton keys).
//! - [`SpatialGrid`]: buckets entity ids into cells so a viewer can gather the ids
//!   near it without scanning the whole world. Rebuilt each tick; allocation-light.
//! - [`VisibilitySet`]: a dense bitset of who is visible to one client, with a
//!   fast bitwise diff against the previous tick, the `entered`/`left` streams a
//!   client needs to spawn and despawn entities.
//!
//! What stays the app's: the world layout (cell size, origin), the relevance rule
//! (a radius, a frustum, a team), and how the streams are encoded on the wire.
//! This only makes those cheap to compute.

use std::collections::HashMap;

/// Z-order (Morton) curve encoding: interleave the bits of integer coordinates
/// into one integer whose ordering follows spatial locality.
///
/// Two cells close in space have (mostly) close Morton codes, so sorting entities
/// by their code groups neighbours, and a cell's code is a single `u64` key for a
/// hash bucket. This is the primitive [`GridQuantizer`] and [`SpatialGrid`] build
/// on; it is public for locality sorts, broadphase, and out-of-core layouts.
pub mod morton {
  /// Spreads the low 32 bits of `v` across the even bit positions of a `u64`
  /// (one zero between each), so two of them interleave into a 64-bit code.
  fn part1by1(mut v: u64) -> u64 {
    v &= 0x0000_0000_ffff_ffff;
    v = (v | (v << 16)) & 0x0000_ffff_0000_ffff;
    v = (v | (v << 8)) & 0x00ff_00ff_00ff_00ff;
    v = (v | (v << 4)) & 0x0f0f_0f0f_0f0f_0f0f;
    v = (v | (v << 2)) & 0x3333_3333_3333_3333;
    v = (v | (v << 1)) & 0x5555_5555_5555_5555;
    v
  }

  /// Inverse of [`part1by1`]: gathers the even bits back into the low 32.
  fn compact1by1(mut v: u64) -> u64 {
    v &= 0x5555_5555_5555_5555;
    v = (v | (v >> 1)) & 0x3333_3333_3333_3333;
    v = (v | (v >> 2)) & 0x0f0f_0f0f_0f0f_0f0f;
    v = (v | (v >> 4)) & 0x00ff_00ff_00ff_00ff;
    v = (v | (v >> 8)) & 0x0000_ffff_0000_ffff;
    v = (v | (v >> 16)) & 0x0000_0000_ffff_ffff;
    v
  }

  /// Interleaves two full 32-bit coordinates into a 64-bit Morton code.
  pub fn encode_2d(x: u32, y: u32) -> u64 {
    part1by1(x as u64) | (part1by1(y as u64) << 1)
  }

  /// Recovers the two coordinates from a 2D Morton code.
  pub fn decode_2d(code: u64) -> (u32, u32) {
    (compact1by1(code) as u32, compact1by1(code >> 1) as u32)
  }

  /// Spreads the low 21 bits of `v` with two zeros between each bit, for 3D.
  fn part1by2(mut v: u64) -> u64 {
    v &= 0x1f_ffff;
    v = (v | (v << 32)) & 0x001f_0000_0000_ffff;
    v = (v | (v << 16)) & 0x001f_0000_ff00_00ff;
    v = (v | (v << 8)) & 0x100f_00f0_0f00_f00f;
    v = (v | (v << 4)) & 0x10c3_0c30_c30c_30c3;
    v = (v | (v << 2)) & 0x1249_2492_4924_9249;
    v
  }

  /// Inverse of [`part1by2`].
  fn compact1by2(mut v: u64) -> u64 {
    v &= 0x1249_2492_4924_9249;
    v = (v | (v >> 2)) & 0x10c3_0c30_c30c_30c3;
    v = (v | (v >> 4)) & 0x100f_00f0_0f00_f00f;
    v = (v | (v >> 8)) & 0x001f_0000_ff00_00ff;
    v = (v | (v >> 16)) & 0x001f_0000_0000_ffff;
    v = (v | (v >> 32)) & 0x1f_ffff;
    v
  }

  /// Interleaves three coordinates into a 64-bit Morton code.
  ///
  /// Three axes share 63 bits, so each coordinate uses its low **21 bits**
  /// (`0..=2_097_151`); higher bits are dropped. That is ample for a cell grid.
  pub fn encode_3d(x: u32, y: u32, z: u32) -> u64 {
    part1by2(x as u64) | (part1by2(y as u64) << 1) | (part1by2(z as u64) << 2)
  }

  /// Recovers the three coordinates from a 3D Morton code.
  pub fn decode_3d(code: u64) -> (u32, u32, u32) {
    (compact1by2(code) as u32, compact1by2(code >> 1) as u32, compact1by2(code >> 2) as u32)
  }
}

/// Maps continuous world coordinates onto a uniform integer grid.
///
/// `origin` should be the world's minimum corner so every coordinate lands in a
/// non-negative cell; anything at or below the origin clamps to cell 0. `cell_size`
/// is the width of a cell in world units, pick it near a viewer's relevance radius
/// so a view spans only a few cells.
#[derive(Debug, Clone, Copy)]
pub struct GridQuantizer {
  origin_x: f32,
  origin_y: f32,
  cell_size: f32,
}

impl GridQuantizer {
  /// # Panics
  /// Panics if `cell_size` is not positive.
  pub fn new(origin: (f32, f32), cell_size: f32) -> Self {
    if cell_size <= 0.0 || cell_size.is_nan() {
      panic!("GridQuantizer cell_size must be positive");
    }
    Self {
      origin_x: origin.0,
      origin_y: origin.1,
      cell_size,
    }
  }

  /// The integer cell a world point falls in.
  pub fn cell(&self, x: f32, y: f32) -> (u32, u32) {
    let cx = ((x - self.origin_x) / self.cell_size).floor().max(0.0);
    let cy = ((y - self.origin_y) / self.cell_size).floor().max(0.0);
    (cx as u32, cy as u32)
  }

  /// The Morton key of the cell a world point falls in.
  pub fn key(&self, x: f32, y: f32) -> u64 {
    let (cx, cy) = self.cell(x, y);
    morton::encode_2d(cx, cy)
  }

  /// How many cells a radius spans, for sizing a query's cell window.
  pub fn cells_for_radius(&self, radius: f32) -> u32 {
    (radius / self.cell_size).ceil().max(0.0) as u32
  }

  pub fn cell_size(&self) -> f32 {
    self.cell_size
  }
}

/// Buckets entity ids into grid cells so a viewer can gather nearby ids without
/// scanning the world.
///
/// Rebuild it each tick ([`clear`](Self::clear) then [`insert`](Self::insert)
/// every entity); the buckets reuse their allocations across ticks, so a steady
/// entity count does not churn the heap. A query returns every id in the cells
/// overlapping the region, a *superset* of what is truly in range (cell-granular),
/// so apply an exact distance test afterward if you need one.
///
/// - `Id`: the entity handle (`u32` index, `u64`, `Uuid`, ...). `Copy`.
#[derive(Debug, Clone)]
pub struct SpatialGrid<Id: Copy> {
  quantizer: GridQuantizer,
  cells: HashMap<u64, Vec<Id>>,
}

impl<Id: Copy> SpatialGrid<Id> {
  pub fn new(quantizer: GridQuantizer) -> Self {
    Self {
      quantizer,
      cells: HashMap::new(),
    }
  }

  /// Empties every bucket but keeps their capacity, so the next tick's inserts do
  /// not reallocate. Call before re-inserting the world each tick.
  pub fn clear(&mut self) {
    for bucket in self.cells.values_mut() {
      bucket.clear();
    }
  }

  /// Files `id` under the cell its position falls in.
  pub fn insert(&mut self, id: Id, x: f32, y: f32) {
    let key = self.quantizer.key(x, y);
    self.cells.entry(key).or_default().push(id);
  }

  /// Appends to `out` every id in the cells overlapping the square of half-width
  /// `radius` around `(x, y)`. Cell-granular, so it can include ids just outside
  /// the radius; filter exactly afterward if that matters. `out` is not cleared,
  /// so a caller can gather several regions, and reusing one `Vec` avoids
  /// allocating per query.
  pub fn query_radius(&self, x: f32, y: f32, radius: f32, out: &mut Vec<Id>) {
    let (cx, cy) = self.quantizer.cell(x, y);
    let r = self.quantizer.cells_for_radius(radius);
    let (x0, x1) = (cx.saturating_sub(r), cx.saturating_add(r));
    let (y0, y1) = (cy.saturating_sub(r), cy.saturating_add(r));
    for gy in y0..=y1 {
      for gx in x0..=x1 {
        if let Some(bucket) = self.cells.get(&morton::encode_2d(gx, gy)) {
          out.extend_from_slice(bucket);
        }
      }
    }
  }

  /// The quantizer this grid uses, for computing keys or cell spans directly.
  pub fn quantizer(&self) -> &GridQuantizer {
    &self.quantizer
  }
}

/// A dense bitset of which entities are visible to one client, with a fast diff
/// against the previous tick.
///
/// Interest management needs, per client, the set of entities in view and the
/// *change* since last tick: who [`entered`](Self::diff) (spawn it) and who
/// [`left`](Self::diff) (despawn it). With entities addressed by a dense `u32`
/// index (recycle indices for short-lived swarm entities, exactly the horde case),
/// that set is a bitset and the diff is a word-at-a-time `new & !old` / `old & !new`,
/// no per-entity hashing.
///
/// For sparse handles (`Uuid`), map them to dense indices first, or diff two
/// sorted id lists instead; this type is the dense-index fast path.
#[derive(Debug, Clone, Default)]
pub struct VisibilitySet {
  words: Vec<u64>,
}

impl VisibilitySet {
  pub fn new() -> Self {
    Self { words: Vec::new() }
  }

  /// Pre-sizes for entity indices up to `max_index`, so inserts up to it do not
  /// reallocate.
  pub fn with_capacity(max_index: u32) -> Self {
    Self {
      words: vec![0; (max_index as usize / 64) + 1],
    }
  }

  /// Empties the set but keeps its capacity, for reuse each tick.
  pub fn clear(&mut self) {
    for w in &mut self.words {
      *w = 0;
    }
  }

  /// Marks entity `index` visible.
  pub fn insert(&mut self, index: u32) {
    let (w, bit) = (index as usize / 64, index as usize % 64);
    if w >= self.words.len() {
      self.words.resize(w + 1, 0);
    }
    self.words[w] |= 1u64 << bit;
  }

  /// Marks entity `index` not visible.
  ///
  /// Useful when an entity is destroyed rather than merely leaving range: clear
  /// it explicitly so the next diff treats a *reused* slot as a fresh arrival
  /// instead of silently carrying the old occupant's membership forward.
  pub fn remove(&mut self, index: u32) {
    let (w, bit) = (index as usize / 64, index as usize % 64);
    if let Some(word) = self.words.get_mut(w) {
      *word &= !(1u64 << bit);
    }
  }

  /// Whether entity `index` is visible.
  pub fn contains(&self, index: u32) -> bool {
    let (w, bit) = (index as usize / 64, index as usize % 64);
    self.words.get(w).is_some_and(|word| word & (1u64 << bit) != 0)
  }

  /// The change from `previous` to `self`: appends newly-visible indices to
  /// `entered` (`self & !previous`) and no-longer-visible ones to `left`
  /// (`previous & !self`), each ascending. The vectors are not cleared, so reuse
  /// them across ticks to avoid allocating. This is the spawn/despawn stream.
  pub fn diff(&self, previous: &VisibilitySet, entered: &mut Vec<u32>, left: &mut Vec<u32>) {
    let n = self.words.len().max(previous.words.len());
    for w in 0..n {
      let cur = self.words.get(w).copied().unwrap_or(0);
      let prev = previous.words.get(w).copied().unwrap_or(0);
      emit_bits(cur & !prev, w, entered);
      emit_bits(prev & !cur, w, left);
    }
  }

  /// The visible indices, ascending.
  pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
    self.words.iter().enumerate().flat_map(|(w, &word)| BitIter { word, base: (w * 64) as u32 })
  }

  /// An order-independent digest of the visible indices, for checking that a
  /// client's mirror of this set still matches. See [`SetDigest`]; feed richer
  /// keys through that directly if membership alone is too weak a check.
  pub fn digest(&self) -> u64 {
    SetDigest::from_keys(self.iter().map(u64::from)).digest()
  }

  /// How many entities are visible.
  pub fn count(&self) -> usize {
    self.words.iter().map(|w| w.count_ones() as usize).sum()
  }

  #[cfg(test)]
  fn from_iter_for_test<I: IntoIterator<Item = u32>>(ids: I) -> Self {
    let mut v = Self::new();
    for i in ids {
      v.insert(i);
    }
    v
  }
}

// The digest is shared with the client crate, which is the point of it: one fold,
// so a disagreement can only ever be about the world and never about the
// arithmetic. Re-exported here because this is where server code reaches for it.
pub use plaza_client_utils::digest::SetDigest;

/// Appends the set bit indices of `word` (in the `w`-th 64-bit block) to `out`.
fn emit_bits(mut word: u64, w: usize, out: &mut Vec<u32>) {
  let base = (w * 64) as u32;
  while word != 0 {
    out.push(base + word.trailing_zeros());
    word &= word - 1; // clear the lowest set bit
  }
}

/// Iterates the set bit indices of a single word.
struct BitIter {
  word: u64,
  base: u32,
}

impl Iterator for BitIter {
  type Item = u32;
  fn next(&mut self) -> Option<u32> {
    if self.word == 0 {
      return None;
    }
    let idx = self.base + self.word.trailing_zeros();
    self.word &= self.word - 1;
    Some(idx)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn morton_2d_round_trips_across_the_range() {
    for &(x, y) in &[(0u32, 0u32), (1, 0), (0, 1), (12345, 67890), (u32::MAX, u32::MAX), (0xdead_beef, 0x0bad_f00d)] {
      let code = morton::encode_2d(x, y);
      assert_eq!(morton::decode_2d(code), (x, y), "2d round trip for ({x}, {y})");
    }
  }

  #[test]
  fn morton_3d_round_trips_within_21_bits() {
    let max21 = (1u32 << 21) - 1;
    for &(x, y, z) in &[(0u32, 0u32, 0u32), (1, 2, 3), (max21, max21, max21), (100_000, 5, 2_000_000)] {
      let code = morton::encode_3d(x, y, z);
      assert_eq!(morton::decode_3d(code), (x, y, z), "3d round trip for ({x}, {y}, {z})");
    }
  }

  #[test]
  fn morton_preserves_locality_within_a_cell_row() {
    // Adjacent cells on a row have codes that bracket their neighbours: the point
    // of a Z-order curve. Not a total order over 2D, but locality holds locally.
    let a = morton::encode_2d(10, 10);
    let b = morton::encode_2d(11, 10);
    let far = morton::encode_2d(10, 1000);
    assert!((a as i64 - b as i64).abs() < (a as i64 - far as i64).abs(), "neighbours are closer in code than a distant cell");
  }

  #[test]
  fn the_quantizer_maps_points_to_cells_and_clamps_below_the_origin() {
    let q = GridQuantizer::new((0.0, 0.0), 10.0);
    assert_eq!(q.cell(5.0, 5.0), (0, 0));
    assert_eq!(q.cell(15.0, 25.0), (1, 2));
    assert_eq!(q.cell(-4.0, -100.0), (0, 0), "below the origin clamps to cell 0");
    assert_eq!(q.cells_for_radius(25.0), 3, "25 world units spans 3 cells of 10");
  }

  #[test]
  #[should_panic]
  fn a_non_positive_cell_size_panics() {
    let _ = GridQuantizer::new((0.0, 0.0), 0.0);
  }

  #[test]
  fn the_grid_returns_ids_near_a_point_and_omits_distant_ones() {
    let q = GridQuantizer::new((0.0, 0.0), 10.0);
    let mut grid = SpatialGrid::new(q);
    grid.insert(1u32, 5.0, 5.0); // cell (0,0)
    grid.insert(2, 12.0, 5.0); // cell (1,0), adjacent
    grid.insert(3, 500.0, 500.0); // far away

    let mut out = Vec::new();
    grid.query_radius(5.0, 5.0, 10.0, &mut out); // one cell of slack each way
    assert!(out.contains(&1) && out.contains(&2), "near ids returned: {out:?}");
    assert!(!out.contains(&3), "distant id omitted");
  }

  #[test]
  fn clearing_the_grid_keeps_capacity_and_empties_buckets() {
    let q = GridQuantizer::new((0.0, 0.0), 10.0);
    let mut grid = SpatialGrid::new(q);
    grid.insert(1u32, 5.0, 5.0);
    grid.clear();
    let mut out = Vec::new();
    grid.query_radius(5.0, 5.0, 10.0, &mut out);
    assert!(out.is_empty(), "cleared grid returns nothing");
  }

  #[test]
  fn a_visibility_set_inserts_and_contains() {
    let mut v = VisibilitySet::new();
    v.insert(3);
    v.insert(70); // second word
    assert!(v.contains(3) && v.contains(70));
    assert!(!v.contains(4));
    assert_eq!(v.count(), 2);
    let seen: Vec<u32> = v.iter().collect();
    assert_eq!(seen, vec![3, 70], "iterates ascending across words");
  }

  #[test]
  fn a_digest_ignores_order_but_not_contents() {
    let a = SetDigest::from_keys([7u64, 1, 99, 4]);
    let b = SetDigest::from_keys([99u64, 4, 7, 1]);
    assert_eq!(a.digest(), b.digest(), "the same set in a different order agrees");

    let c = SetDigest::from_keys([7u64, 1, 99, 5]);
    assert_ne!(a.digest(), c.digest(), "a different member disagrees");

    let d = SetDigest::from_keys([7u64, 1, 99]);
    assert_ne!(a.digest(), d.digest(), "a missing member disagrees");
  }

  #[test]
  fn maintaining_a_digest_incrementally_matches_rebuilding_it() {
    let mut live = SetDigest::new();
    for k in [3u64, 9, 21, 40] {
      live.insert(k);
    }
    live.remove(9);
    live.insert(77);

    let rebuilt = SetDigest::from_keys([3u64, 21, 40, 77]);
    assert_eq!(live.digest(), rebuilt.digest(), "O(1) updates agree with a full rebuild");
    assert_eq!(live.len(), 4);
  }

  #[test]
  fn a_delta_that_never_landed_is_detectable() {
    // The failure this exists for: the server removed an entity, the client
    // never applied it, and nothing else about the client looks wrong.
    let mut server = SetDigest::from_keys([1u64, 2, 3, 4]);
    let client = SetDigest::from_keys([1u64, 2, 3, 4]);
    assert_eq!(server.digest(), client.digest());

    server.remove(3); // the client misses this despawn
    assert_ne!(server.digest(), client.digest(), "the mirror is stale and it shows");
  }

  #[test]
  fn a_visibility_set_digest_tracks_its_membership() {
    let mut v = VisibilitySet::new();
    for i in [2u32, 8, 300] {
      v.insert(i);
    }
    let before = v.digest();
    v.remove(8);
    assert_ne!(before, v.digest());

    let same = VisibilitySet::from_iter_for_test([2u32, 300]);
    assert_eq!(v.digest(), same.digest(), "membership alone decides the digest");
  }

  #[test]
  fn removing_clears_a_bit_so_a_reused_slot_reads_as_a_fresh_arrival() {
    let mut v = VisibilitySet::new();
    v.insert(5);
    v.remove(5);
    assert!(!v.contains(5));

    // The point: with the bit cleared, the next diff sees 5 as newly visible
    // rather than carrying the dead occupant's membership forward.
    let mut next = VisibilitySet::new();
    next.insert(5);
    let (mut entered, mut left) = (Vec::new(), Vec::new());
    next.diff(&v, &mut entered, &mut left);
    assert_eq!(entered, vec![5]);
    assert!(left.is_empty());
  }

  #[test]
  fn the_visibility_diff_reports_spawns_and_despawns() {
    // Previous tick saw {1, 2, 3}; this tick sees {2, 3, 4, 100}.
    let mut prev = VisibilitySet::new();
    for i in [1, 2, 3] {
      prev.insert(i);
    }
    let mut cur = VisibilitySet::new();
    for i in [2, 3, 4, 100] {
      cur.insert(i);
    }

    let (mut entered, mut left) = (Vec::new(), Vec::new());
    cur.diff(&prev, &mut entered, &mut left);
    assert_eq!(entered, vec![4, 100], "newly visible: spawn these");
    assert_eq!(left, vec![1], "no longer visible: despawn these");
  }

  #[test]
  fn a_full_relevance_pass_matches_a_brute_force_check() {
    // The grid query plus an exact distance test should select exactly the ids a
    // naive all-pairs scan would, for a random-ish spread of entities.
    let q = GridQuantizer::new((0.0, 0.0), 32.0);
    let mut grid = SpatialGrid::new(q);
    let radius = 50.0;

    // Deterministic pseudo-spread of 200 entities over a 1000x1000 field.
    let entities: Vec<(u32, f32, f32)> = (0..200u32)
      .map(|i| {
        let x = ((i * 73 % 1000) as f32) + 0.5;
        let y = ((i * 149 % 1000) as f32) + 0.5;
        (i, x, y)
      })
      .collect();
    for &(id, x, y) in &entities {
      grid.insert(id, x, y);
    }

    let (vx, vy) = (500.0f32, 500.0f32); // a viewer in the middle
    let brute: std::collections::BTreeSet<u32> = entities
      .iter()
      .filter(|(_, x, y)| ((x - vx).powi(2) + (y - vy).powi(2)).sqrt() <= radius)
      .map(|(id, _, _)| *id)
      .collect();

    // Grid gathers a cell-granular superset; the exact test narrows it.
    let mut candidates = Vec::new();
    grid.query_radius(vx, vy, radius, &mut candidates);
    let grid_result: std::collections::BTreeSet<u32> = candidates
      .into_iter()
      .filter(|id| {
        let (_, x, y) = entities[*id as usize];
        ((x - vx).powi(2) + (y - vy).powi(2)).sqrt() <= radius
      })
      .collect();

    assert_eq!(grid_result, brute, "grid + exact test equals brute force");
  }
}
