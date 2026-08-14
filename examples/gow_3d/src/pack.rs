//! The zone's hot array, written by hand into bits.
//!
//! `wire_cost` measures the frame and `examples/zone_scale.rs` measures the
//! tick, and past a few hundred connected clients the encode is half of it.
//! This is the part a derive cannot reach: serde knows a position is three
//! `f32`, it does not know the zone is 240 units across and renders at a
//! millimetre. Every choice here is a bound plus a precision, and both are
//! properties of *this* zone that no codec could infer.
//!
//! The envelope stays MessagePack. Only the array every client is sent every
//! tick gets this treatment, which is the same division cube_yard and spacemo
//! arrived at, and for the same reason: a layout and a reader are two functions
//! that must agree with nothing but this module holding them together.
//!
//! **The bounds are wider than the map and asserted against it.** spacemo
//! shipped a bound sized to the one path a body could reach the packet by, and
//! a second path opened later and clamped silently: a ship flying perfectly
//! well on the server, pinned to a wall on every client. Nothing here is sized
//! to `terrain::EDGE` exactly, and the assertions below are what say so.

use plaza_wire::bits::{BitReader, BitWriter};

use crate::protocol::{Because, Kind, Seen};
use crate::terrain;

/// Horizontal bounds, wide enough that walking off the map is representable.
const XZ: (f32, f32) = (-512.0, 512.0);
const XZ_BITS: u32 = 18;

/// Vertical bounds: the sea floor to well above the tallest hill.
const Y: (f32, f32) = (-64.0, 192.0);
const Y_BITS: u32 = 16;

const YAW: (f32, f32) = (-std::f32::consts::PI, std::f32::consts::PI);
const YAW_BITS: u32 = 10;

/// Longest cast bar that can be described, in milliseconds.
const CASTING_BITS: u32 = 14;

const _: () = assert!(XZ.1 > terrain::EDGE, "the wire must reach past the map, not to it");
const _: () = assert!(Y.1 > terrain::RELIEF, "the wire must reach over the tallest hill");

/// How far outside its own cell a body may still be described.
///
/// A body is packed from the same positions the index was built from in the
/// same tick, so it is *in* its cell; the exception is one clamped into a
/// border cell because the index is smaller than the world, which is a
/// pathology with its own test. Half a cell each way is headroom for that
/// without paying for range nobody uses.
const REL_PAD: f32 = crate::zone::CELL / 2.0;
/// The horizontal range a cell-relative axis covers: the cell plus its padding.
const REL_RANGE: f32 = crate::zone::CELL + REL_PAD * 2.0;

/// Horizontal bits when a payload knows which cell it describes.
///
/// **Five fewer than absolute, not six.** The first attempt used 12 and padded
/// the range by a whole cell each way, which tripled the range to 46 units and
/// made the "cheaper *and* finer" claim simply false: 12 bits over 46 is
/// coarser than 18 over 1024. The const assertion below is what caught it.
pub const REL_BITS: u32 = 13;

const _: () = assert!(
  (REL_RANGE as f64) / ((1u32 << REL_BITS) - 1) as f64
    <= ((XZ.1 - XZ.0) as f64) / ((1u32 << XZ_BITS) - 1) as f64,
  "cell-relative must not be coarser than the absolute layout it replaces"
);

/// Writes one body with positions relative to `corner`, the world-space
/// minimum corner of the cell carrying it.
///
/// A body can sit fractionally outside its own cell between the index being
/// built and the payload being packed, so the range is padded by a whole cell
/// each way rather than clamped to the cell exactly: the same lesson as the
/// wire bounds being wider than the map.
pub fn write_in_cell(w: &mut BitWriter, seen: &Seen, corner: (f32, f32)) {
  write_in_cell_at(w, seen, corner, REL_BITS);
}

/// [`read`] for a body written by [`write_in_cell`].
pub fn read_in_cell(r: &mut BitReader, corner: (f32, f32), because: Because) -> Option<Seen> {
  read_in_cell_at(r, corner, REL_BITS, because)
}

pub fn write(w: &mut BitWriter, seen: &Seen) {
  w.bits(seen.seat as u64, 16);
  w.quantized(seen.at.0, XZ.0, XZ.1, XZ_BITS);
  w.quantized(seen.at.1, Y.0, Y.1, Y_BITS);
  w.quantized(seen.at.2, XZ.0, XZ.1, XZ_BITS);
  w.varint(seen.health as u64);
  w.varint(seen.max_health as u64);
  w.quantized(seen.yaw, YAW.0, YAW.1, YAW_BITS);
  w.bits(seen.kind as u64, 2);
  match seen.casting_ms {
    Some(ms) => {
      w.bool(true);
      w.bits(u64::from(ms).min((1 << CASTING_BITS) - 1), CASTING_BITS);
    }
    None => w.bool(false),
  }
}

/// Reads one character, stamping `because`: the layout does not carry it,
/// since which channel a payload arrived on already says why its entries are
/// in the frame, and a payload shared by every viewer of a cell could not
/// carry a per-viewer answer anyway.
pub fn read(r: &mut BitReader, because: Because) -> Option<Seen> {
  let seat = r.bits(16).ok()? as u16;
  let x = r.quantized(XZ.0, XZ.1, XZ_BITS).ok()?;
  let y = r.quantized(Y.0, Y.1, Y_BITS).ok()?;
  let z = r.quantized(XZ.0, XZ.1, XZ_BITS).ok()?;
  let health = r.varint().ok()? as u16;
  let max_health = r.varint().ok()? as u16;
  let yaw = r.quantized(YAW.0, YAW.1, YAW_BITS).ok()?;
  let kind = match r.bits(2).ok()? {
    0 => Kind::Adventurer,
    _ => Kind::Beast,
  };
  let casting_ms = if r.bool().ok()? {
    Some(r.bits(CASTING_BITS).ok()? as u32)
  } else {
    None
  };
  Some(Seen {
    seat,
    at: (x, y, z),
    health,
    max_health,
    yaw,
    kind,
    because,
    casting_ms,
  })
}

/// Opens an audience with the count it is about to write.
///
/// A count rather than reading until the bits run out: a record is not a fixed
/// width (two varints and an optional cast bar move it), so "is there room for
/// another" cannot be asked without already knowing the answer, and a reader
/// that guesses drops the last character on some frames and not others.
pub fn open(w: &mut BitWriter, count: usize) {
  w.varint(count as u64);
}

/// Bits a cell far enough away can drop to and stay under a pixel.
///
/// Three fewer than [`REL_BITS`], which at the view edge is still well inside
/// one pixel because the error a player can see shrinks with distance.
pub const GRADED_COARSE_BITS: u32 = REL_BITS - 3;

/// Opens a graded payload: the cell it describes, its count, and **which width
/// it used**.
///
/// The tag is on the payload rather than agreed out of band because the width
/// is chosen per cell and a reader cannot guess it. It cannot be chosen per
/// *viewer*, which is the constraint that shapes this whole scheme: a cell is
/// packed once and shared, so a per-viewer width is exactly as impossible here
/// as a per-viewer `Because`. The zone publishes both widths and each viewer
/// takes the one its own distance earns.
pub fn open_graded(w: &mut BitWriter, index: usize, count: usize, coarse: bool) {
  w.varint(index as u64);
  w.varint(count as u64);
  w.bool(coarse);
}

/// [`write_in_cell`] at an explicit width.
pub fn write_in_cell_at(w: &mut BitWriter, seen: &Seen, corner: (f32, f32), bits: u32) {
  w.bits(seen.seat as u64, 16);
  w.quantized(seen.at.0, corner.0 - REL_PAD, corner.0 - REL_PAD + REL_RANGE, bits);
  w.quantized(seen.at.1, Y.0, Y.1, Y_BITS);
  w.quantized(seen.at.2, corner.1 - REL_PAD, corner.1 - REL_PAD + REL_RANGE, bits);
  w.varint(seen.health as u64);
  w.varint(seen.max_health as u64);
  w.quantized(seen.yaw, YAW.0, YAW.1, YAW_BITS);
  w.bits(seen.kind as u64, 2);
  match seen.casting_ms {
    Some(ms) => {
      w.bool(true);
      w.bits(u64::from(ms).min((1 << CASTING_BITS) - 1), CASTING_BITS);
    }
    None => w.bool(false),
  }
}

/// [`read_in_cell`] at an explicit width.
pub fn read_in_cell_at(r: &mut BitReader, corner: (f32, f32), bits: u32, because: Because) -> Option<Seen> {
  let seat = r.bits(16).ok()? as u16;
  let x = r.quantized(corner.0 - REL_PAD, corner.0 - REL_PAD + REL_RANGE, bits).ok()?;
  let y = r.quantized(Y.0, Y.1, Y_BITS).ok()?;
  let z = r.quantized(corner.1 - REL_PAD, corner.1 - REL_PAD + REL_RANGE, bits).ok()?;
  let health = r.varint().ok()? as u16;
  let max_health = r.varint().ok()? as u16;
  let yaw = r.quantized(YAW.0, YAW.1, YAW_BITS).ok()?;
  let kind = match r.bits(2).ok()? {
    0 => Kind::Adventurer,
    _ => Kind::Beast,
  };
  let casting_ms = if r.bool().ok()? {
    Some(r.bits(CASTING_BITS).ok()? as u32)
  } else {
    None
  };
  Some(Seen { seat, at: (x, y, z), health, max_health, yaw, kind, because, casting_ms })
}

/// Unpacks a run of graded payloads, taking each one's width from its own tag.
pub fn unpack_graded_into(
  bytes: &[u8],
  space: &plaza_server_utils::relevance::CellSpace,
  because: Because,
  out: &mut Vec<Seen>,
) {
  let mut r = BitReader::new(bytes);
  while r.bits_left() >= 8 {
    let Ok(index) = r.varint() else { return };
    let Ok(count) = r.varint() else { return };
    let Ok(coarse) = r.bool() else { return };
    let bits = if coarse { GRADED_COARSE_BITS } else { REL_BITS };
    let corner = space.corner(index as usize);
    out.reserve(count as usize);
    for _ in 0..count {
      let Some(seen) = read_in_cell_at(&mut r, corner, bits, because) else { return };
      out.push(seen);
    }
    r.align_to_byte();
  }
}

/// Opens a cell-relative payload, which must name the cell it describes.
///
/// That name is the cost of the scheme: a reader turns the index into a corner
/// through the same [`CellSpace`](plaza_server_utils::relevance::CellSpace) the
/// server used, and cannot decode a body without it. It is charged once per
/// cell against a saving of twelve bits per body, so the trade turns entirely
/// on how many bodies a cell holds.
pub fn open_cell(w: &mut BitWriter, index: usize, count: usize) {
  w.varint(index as u64);
  w.varint(count as u64);
}

/// Unpacks a run of cell-relative payloads, resolving each cell's corner
/// through `space`.
pub fn unpack_cells_into(
  bytes: &[u8],
  space: &plaza_server_utils::relevance::CellSpace,
  because: Because,
  out: &mut Vec<Seen>,
) {
  let mut r = BitReader::new(bytes);
  while r.bits_left() >= 8 {
    let Ok(index) = r.varint() else { return };
    let Ok(count) = r.varint() else { return };
    let corner = space.corner(index as usize);
    out.reserve(count as usize);
    for _ in 0..count {
      let Some(seen) = read_in_cell(&mut r, corner, because) else { return };
      out.push(seen);
    }
    r.align_to_byte();
  }
}

/// Everyone in one payload, unpacked, all stamped `because`.
pub fn unpack(bytes: &[u8], because: Because) -> Vec<Seen> {
  let mut out = Vec::new();
  unpack_into(bytes, because, &mut out);
  out
}

/// [`unpack`] appending to a caller's buffer, reading **as many payloads as
/// the buffer holds**.
///
/// One cell's payload is one count and its records. `Delivery::Joined` hands a
/// client the payloads of every cell its view touches concatenated, which works
/// precisely because each opens with its own count: the reader takes them in
/// turn until the bytes run out. Each was byte-padded by `finish`, so the
/// reader realigns between them.
pub fn unpack_into(bytes: &[u8], because: Because, out: &mut Vec<Seen>) {
  let mut r = BitReader::new(bytes);
  // A trailing partial byte is padding, never another payload.
  while r.bits_left() >= 8 {
    let Ok(count) = r.varint() else { return };
    out.reserve(count as usize);
    for _ in 0..count {
      let Some(seen) = read(&mut r, because) else { return };
      out.push(seen);
    }
    r.align_to_byte();
  }
}

/// Packs a whole audience, for a caller that already has the slice.
pub fn pack(crowd: &[Seen]) -> Vec<u8> {
  let mut w = BitWriter::new();
  open(&mut w, crowd.len());
  for one in crowd {
    write(&mut w, one);
  }
  w.finish()
}

#[cfg(test)]
mod tests {
  use super::*;

  fn seen(seat: u16, at: (f32, f32, f32)) -> Seen {
    Seen {
      seat,
      at,
      health: 73,
      max_health: 100,
      yaw: 1.25,
      kind: Kind::Beast,
      because: Because::BothOfThose,
      casting_ms: Some(1500),
    }
  }

  #[test]
  fn a_character_survives_its_own_reader() {
    let original = seen(4095, (61.5, 12.25, -88.0));
    let bytes = pack(&[original]);

    let back = unpack(&bytes, Because::BothOfThose);
    assert_eq!(back.len(), 1);
    let back = back[0];
    assert_eq!(back.seat, original.seat);
    assert_eq!(back.health, original.health);
    assert_eq!(back.max_health, original.max_health);
    assert_eq!(back.kind, original.kind);
    assert_eq!(back.because, Because::BothOfThose, "the reader's stamp, not the wire's");
    assert_eq!(back.casting_ms, original.casting_ms);

    // Quantisation is the trade this module makes, so the error is asserted
    // rather than described: a step is the range over the codes it has.
    let xz_step = (XZ.1 - XZ.0) / ((1u32 << XZ_BITS) - 1) as f32;
    assert!((back.at.0 - original.at.0).abs() <= xz_step);
    assert!((back.at.2 - original.at.2).abs() <= xz_step);
    let y_step = (Y.1 - Y.0) / ((1u32 << Y_BITS) - 1) as f32;
    assert!((back.at.1 - original.at.1).abs() <= y_step);
    let yaw_step = (YAW.1 - YAW.0) / ((1u32 << YAW_BITS) - 1) as f32;
    assert!((back.yaw - original.yaw).abs() <= yaw_step);
  }

  #[test]
  fn a_body_past_the_map_still_lands_where_it_stands() {
    // The spacemo defect, as a test rather than a comment: the wire bound is
    // wider than the world, so a character out past the coastline is described
    // rather than pinned to the edge of what the layout could say.
    let far = terrain::EDGE + 120.0;
    let back = unpack(&pack(&[seen(9, (far, 0.0, -far))]), Because::Near);
    let xz_step = (XZ.1 - XZ.0) / ((1u32 << XZ_BITS) - 1) as f32;
    assert!((back[0].at.0 - far).abs() <= xz_step, "clamped at {}", back[0].at.0);
    assert!((back[0].at.2 + far).abs() <= xz_step);
  }

  #[test]
  fn a_cell_relative_body_lands_where_it_stands_and_costs_less() {
    // The claim: quantising over one cell instead of the world is fewer bits
    // at a *finer* step, so this is a saving with no fidelity trade at all.
    // Both halves are asserted, because a cheaper wire that moved bodies would
    // not be a saving.
    let corner = (30.0, -45.0);
    let body = seen(11, (corner.0 + 4.25, 6.5, corner.1 + 11.75));

    let mut w = BitWriter::new();
    open_cell(&mut w, 7, 1);
    write_in_cell(&mut w, &body, corner);
    let relative = w.finish();

    let mut w = BitWriter::new();
    open(&mut w, 1);
    write(&mut w, &body);
    let absolute = w.finish();

    assert!(
      relative.len() <= absolute.len(),
      "cell-relative was bigger: {} against {}",
      relative.len(),
      absolute.len()
    );

    let mut r = BitReader::new(&relative);
    assert_eq!(r.varint().unwrap(), 7, "the payload names its cell");
    assert_eq!(r.varint().unwrap(), 1);
    let back = read_in_cell(&mut r, corner, Because::Near).expect("a body");
    assert_eq!(back.seat, body.seat);
    let step = REL_RANGE / ((1u32 << REL_BITS) - 1) as f32;
    assert!((back.at.0 - body.at.0).abs() <= step, "x drifted {}", back.at.0 - body.at.0);
    assert!((back.at.2 - body.at.2).abs() <= step);
    let absolute_step = (XZ.1 - XZ.0) / ((1u32 << XZ_BITS) - 1) as f32;
    assert!(step <= absolute_step, "the cheaper layout must not be coarser");
  }

  #[test]
  fn a_body_that_drifts_out_of_its_own_cell_is_still_described() {
    // The spacemo defect once more, at cell scale: the index is built before
    // the payload is packed, so a body can be fractionally outside the cell
    // that carries it. The range is padded a whole cell each way rather than
    // clamped to the cell exactly.
    let corner = (0.0, 0.0);
    let outside = seen(3, (-REL_PAD * 0.9, 2.0, crate::zone::CELL + REL_PAD * 0.9));
    let mut w = BitWriter::new();
    write_in_cell(&mut w, &outside, corner);
    let bytes = w.finish();

    let mut r = BitReader::new(&bytes);
    let back = read_in_cell(&mut r, corner, Because::Near).expect("a body");
    let step = REL_RANGE / ((1u32 << REL_BITS) - 1) as f32;
    assert!((back.at.0 - outside.at.0).abs() <= step, "clamped at {}", back.at.0);
    assert!((back.at.2 - outside.at.2).abs() <= step, "clamped at {}", back.at.2);
  }

  #[test]
  fn a_whole_audience_round_trips_in_order() {
    let crowd: Vec<Seen> = (0..44u16)
      .map(|s| seen(s, (s as f32 - 20.0, 3.0, s as f32 * 0.5)))
      .collect();
    let bytes = pack(&crowd);
    let back = unpack(&bytes, Because::Near);
    assert_eq!(back.len(), crowd.len(), "everyone came back");
    for (a, b) in crowd.iter().zip(&back) {
      assert_eq!(a.seat, b.seat, "and in the order they went in");
    }

    // The number the whole exercise is for, pinned so it cannot drift up
    // unnoticed: MessagePack was measured at about 42 bytes a character.
    let each = bytes.len() as f32 / crowd.len() as f32;
    assert!(each < 16.0, "a character costs {each:.1} bytes packed");
  }
}
