//! Pack each wanted occupied cell once, then deal every pane its cells.
//!
//! The middle layer is keyed by place, never by watcher: the buckets, the
//! payloads and the pane mask are three `CellTable`s over one `CellSpace`,
//! and a cell nobody watches is never packed at all.

use plaza_server_utils::relevance::{CellSpace, CellTable};

use crate::pack;
use crate::protocol::{Packed, PAYLOAD_BUDGET};
use crate::sim::Colony;

/// Ants bucketed by cell, rebuilt every tick without churning the heap.
pub struct Buckets {
  cells: CellTable<Vec<u32>>,
}

impl Buckets {
  pub fn new(space: CellSpace) -> Self {
    Self {
      cells: CellTable::new(space),
    }
  }

  pub fn rebuild(&mut self, colony: &Colony) {
    self.cells.clear_each();
    let space = self.cells.space().clone();
    for i in 0..colony.len() {
      let index = space.index_of(colony.x[i], colony.y[i]);
      if let Some(bucket) = self.cells.get_mut(index) {
        bucket.push(i as u32);
      }
    }
  }

  pub fn members(&self, index: usize) -> &[u32] {
    self.cells.get(index).map(Vec::as_slice).unwrap_or(&[])
  }

  pub fn occupied(&self) -> impl Iterator<Item = (usize, &[u32])> {
    self.cells.occupied().map(|(index, ids)| (index, ids.as_slice()))
  }

  pub fn space(&self) -> &CellSpace {
    self.cells.space()
  }
}

/// The cell indices a square pane touches, clamped to the board.
pub fn pane_cells(space: &CellSpace, x: f32, y: f32, half: f32) -> Vec<usize> {
  let side = space.side();
  let (cx0, cy0) = space.quantizer().cell(x - half, y - half);
  let (cx1, cy1) = space.quantizer().cell(x + half, y + half);
  let (cx1, cy1) = (cx1.min(side - 1), cy1.min(side - 1));
  let mut cells = Vec::with_capacity(((cx1 - cx0 + 1) * (cy1 - cy0 + 1)) as usize);
  for cy in cy0..=cy1 {
    for cx in cx0..=cx1 {
      cells.push(space.index_at(cx, cy));
    }
  }
  cells
}

/// Which cells any pane wants this tick.
pub struct Wanted {
  mask: Vec<bool>,
}

impl Wanted {
  pub fn new(space: &CellSpace) -> Self {
    Self {
      mask: vec![false; space.len()],
    }
  }

  pub fn reset(&mut self) {
    self.mask.fill(false);
  }

  pub fn mark(&mut self, cells: &[usize]) {
    for &cell in cells {
      self.mask[cell] = true;
    }
  }

  pub fn contains(&self, cell: usize) -> bool {
    self.mask.get(cell).copied().unwrap_or(false)
  }
}

/// One tick's packed payloads: `Some` exactly at wanted occupied cells.
pub struct Publication {
  cells: CellTable<Option<Packed>>,
  pub packed_cells: usize,
  pub packed_bytes: usize,
}

impl Publication {
  pub fn new(space: CellSpace) -> Self {
    Self {
      cells: CellTable::new(space),
      packed_cells: 0,
      packed_bytes: 0,
    }
  }

  pub fn publish(&mut self, colony: &Colony, buckets: &Buckets, wanted: &Wanted) {
    self.cells.clear_each();
    self.packed_cells = 0;
    self.packed_bytes = 0;
    let space = self.cells.space().clone();
    for (index, ids) in buckets.occupied() {
      if !wanted.contains(index) {
        continue;
      }
      let mut bytes = Vec::with_capacity(pack::RECORD_HEADER + ids.len() * 2);
      pack::pack_cell(&mut bytes, index as u16, space.corner(index), ids, &colony.x, &colony.y);
      self.packed_cells += 1;
      self.packed_bytes += bytes.len();
      if let Some(slot) = self.cells.get_mut(index) {
        *slot = Some(Packed::new(bytes));
      }
    }
  }

  pub fn cell(&self, index: usize) -> Option<&Packed> {
    self.cells.get(index).and_then(Option::as_ref)
  }
}

/// Deals one pane's cells into payloads no larger than the budget, splitting
/// at record boundaries when one cell alone is bigger than a datagram.
pub fn assemble(publication: &Publication, cells: &[usize], out: &mut Vec<Vec<u8>>) {
  let mut chunk: Vec<u8> = Vec::with_capacity(PAYLOAD_BUDGET);
  for &index in cells {
    let Some(packed) = publication.cell(index) else { continue };
    let bytes = packed.as_slice();
    if bytes.len() <= PAYLOAD_BUDGET - chunk.len() {
      chunk.extend_from_slice(bytes);
      continue;
    }
    for record in pack::records(bytes).flatten() {
      let span = pack::RECORD_HEADER + record.offsets.len();
      if span > PAYLOAD_BUDGET - chunk.len() {
        out.push(std::mem::replace(&mut chunk, Vec::with_capacity(PAYLOAD_BUDGET)));
      }
      let start = chunk.len();
      chunk.extend_from_slice(&record.cell.to_le_bytes());
      chunk.extend_from_slice(&(record.count() as u16).to_le_bytes());
      chunk.extend_from_slice(record.offsets);
      debug_assert!(chunk.len() - start == span);
    }
  }
  if !chunk.is_empty() {
    out.push(chunk);
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::protocol::EXTENT;
  use crate::sim::{board, Colony};

  fn farm(population: usize) -> (Colony, Buckets) {
    let colony = Colony::new(population, EXTENT, 8, 3);
    let mut buckets = Buckets::new(board(EXTENT));
    buckets.rebuild(&colony);
    (colony, buckets)
  }

  #[test]
  fn a_pane_at_the_edge_stays_on_the_board() {
    let space = board(EXTENT);
    let cells = pane_cells(&space, 0.0, 0.0, 64.0);
    assert!(!cells.is_empty());
    for &cell in &cells {
      assert!(cell < space.len());
    }
  }

  #[test]
  fn a_cell_nobody_watches_is_never_packed() {
    let (colony, buckets) = farm(10_000);
    let space = buckets.space().clone();
    let mut wanted = Wanted::new(&space);
    let pane = pane_cells(&space, colony.nest.0, colony.nest.1, 64.0);
    wanted.mark(&pane);

    let mut publication = Publication::new(space);
    publication.publish(&colony, &buckets, &wanted);

    let mut outside_occupied = None;
    for (index, _) in buckets.occupied() {
      if !wanted.contains(index) {
        outside_occupied = Some(index);
        break;
      }
    }
    let outside = outside_occupied.expect("10k ants heading to 8 sites occupy cells outside one pane");
    assert!(publication.cell(outside).is_none());
    assert!(publication.packed_cells > 0);
  }

  #[test]
  fn assembled_payloads_fit_the_budget_and_cover_the_pane() {
    let (colony, buckets) = farm(50_000);
    let space = buckets.space().clone();
    let mut wanted = Wanted::new(&space);
    let pane = pane_cells(&space, colony.nest.0, colony.nest.1, 128.0);
    wanted.mark(&pane);

    let mut publication = Publication::new(space);
    publication.publish(&colony, &buckets, &wanted);

    let mut payloads = Vec::new();
    assemble(&publication, &pane, &mut payloads);

    let mut carried = 0usize;
    for payload in &payloads {
      assert!(payload.len() <= crate::protocol::PAYLOAD_BUDGET);
      for record in crate::pack::records(payload) {
        carried += record.expect("well formed").count();
      }
    }

    let mut expected = 0usize;
    for &cell in &pane {
      expected += buckets.members(cell).len();
    }
    assert_eq!(carried, expected, "every ant in the pane rides exactly one payload");
    assert!(expected > 0);
  }
}
