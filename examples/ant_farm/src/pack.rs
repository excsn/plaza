//! Cell records: `[cell u16 le][count u16 le][dx u8, dy u8]*`.
//!
//! A record is self-delimiting, so records concatenate into one payload with
//! no framing between them, and a crowded cell splits into several records of
//! the same cell rather than ever outgrowing a datagram.

use crate::protocol::{CELL, PAYLOAD_BUDGET};

pub const RECORD_HEADER: usize = 4;

/// The most ants one record may carry, chosen so a single full record plus its
/// header always fits the payload budget.
pub const ANTS_PER_RECORD: usize = (PAYLOAD_BUDGET - RECORD_HEADER) / 2;

/// Packs one cell's ants, splitting into multiple records when the cell holds
/// more than [`ANTS_PER_RECORD`].
pub fn pack_cell(out: &mut Vec<u8>, cell: u16, corner: (f32, f32), ids: &[u32], xs: &[f32], ys: &[f32]) {
  let scale = 256.0 / CELL;
  for chunk in ids.chunks(ANTS_PER_RECORD) {
    out.extend_from_slice(&cell.to_le_bytes());
    out.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
    for &id in chunk {
      let i = id as usize;
      let dx = ((xs[i] - corner.0) * scale).clamp(0.0, 255.0) as u8;
      let dy = ((ys[i] - corner.1) * scale).clamp(0.0, 255.0) as u8;
      out.push(dx);
      out.push(dy);
    }
  }
}

pub struct CellRecord<'a> {
  pub cell: u16,
  pub offsets: &'a [u8],
}

impl CellRecord<'_> {
  pub fn count(&self) -> usize {
    self.offsets.len() / 2
  }

  pub fn positions(&self, corner: (f32, f32)) -> impl Iterator<Item = (f32, f32)> + '_ {
    let scale = CELL / 256.0;
    self.offsets.chunks_exact(2).map(move |pair| {
      (
        corner.0 + (pair[0] as f32 + 0.5) * scale,
        corner.1 + (pair[1] as f32 + 0.5) * scale,
      )
    })
  }
}

/// Walks concatenated records. Returns `None` at a truncated or malformed
/// tail, which on a datagram link means the payload is discarded whole.
pub fn records(bytes: &[u8]) -> Records<'_> {
  Records { bytes }
}

pub struct Records<'a> {
  bytes: &'a [u8],
}

impl<'a> Iterator for Records<'a> {
  type Item = Option<CellRecord<'a>>;

  fn next(&mut self) -> Option<Self::Item> {
    if self.bytes.is_empty() {
      return None;
    }
    if self.bytes.len() < RECORD_HEADER {
      self.bytes = &[];
      return Some(None);
    }
    let cell = u16::from_le_bytes([self.bytes[0], self.bytes[1]]);
    let count = u16::from_le_bytes([self.bytes[2], self.bytes[3]]) as usize;
    let body = RECORD_HEADER + count * 2;
    if self.bytes.len() < body {
      self.bytes = &[];
      return Some(None);
    }
    let record = CellRecord {
      cell,
      offsets: &self.bytes[RECORD_HEADER..body],
    };
    self.bytes = &self.bytes[body..];
    Some(Some(record))
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::protocol::EXTENT;
  use crate::sim::board;

  #[test]
  fn a_cell_round_trips_inside_its_corner() {
    let space = board(EXTENT);
    let index = space.index_of(100.0, 100.0);
    let corner = space.corner(index);
    let (xs, ys) = (vec![100.0, 101.5, 103.9], vec![100.0, 103.2, 97.6]);
    let ids = vec![0u32, 1, 2];

    let mut bytes = Vec::new();
    pack_cell(&mut bytes, index as u16, corner, &ids, &xs, &ys);

    let mut seen = 0;
    for record in records(&bytes) {
      let record = record.expect("well formed");
      assert_eq!(record.cell, index as u16);
      for (i, (px, py)) in record.positions(corner).enumerate() {
        assert!((px - xs[i]).abs() <= CELL / 256.0, "x within one quantum");
        assert!((py - ys[i]).abs() <= CELL / 256.0, "y within one quantum");
        seen += 1;
      }
    }
    assert_eq!(seen, 3);
  }

  #[test]
  fn a_crowded_cell_splits_and_every_record_fits_the_budget() {
    let space = board(EXTENT);
    let index = space.index_of(500.0, 500.0);
    let corner = space.corner(index);
    let n = ANTS_PER_RECORD * 2 + 17;
    let xs: Vec<f32> = (0..n).map(|i| corner.0 + (i % 8) as f32).collect();
    let ys: Vec<f32> = (0..n).map(|i| corner.1 + (i % 7) as f32).collect();
    let ids: Vec<u32> = (0..n as u32).collect();

    let mut bytes = Vec::new();
    pack_cell(&mut bytes, index as u16, corner, &ids, &xs, &ys);

    let mut total = 0;
    let mut record_count = 0;
    for record in records(&bytes) {
      let record = record.expect("well formed");
      assert!(RECORD_HEADER + record.offsets.len() <= PAYLOAD_BUDGET);
      total += record.count();
      record_count += 1;
    }
    assert_eq!(total, n);
    assert_eq!(record_count, 3);
  }

  #[test]
  fn a_truncated_payload_reads_as_malformed_not_as_fewer_ants() {
    let space = board(EXTENT);
    let index = space.index_of(64.0, 64.0);
    let corner = space.corner(index);
    let ids = vec![0u32, 1, 2, 3];
    let (xs, ys) = (vec![64.0; 4], vec![64.0; 4]);

    let mut bytes = Vec::new();
    pack_cell(&mut bytes, index as u16, corner, &ids, &xs, &ys);
    bytes.truncate(bytes.len() - 3);

    let mut saw_malformed = false;
    for record in records(&bytes) {
      if record.is_none() {
        saw_malformed = true;
      }
    }
    assert!(saw_malformed);
  }
}
