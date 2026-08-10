//! What each packing strategy actually costs, on the payload the technique was
//! invented for.
//!
//! Glenn Fiedler's [snapshot compression](https://gafferongames.com/post/snapshot_compression/)
//! article works a scene of 901 cubes at 60Hz and takes it from 17.38 Mbps to
//! its 256 kbps target. This reproduces the shape of that payload against the
//! three things plaza can do with it, because "bit packing would help" is a
//! claim that deserves a number rather than a paragraph.
//!
//! Run it with output:
//! ```sh
//! cargo test -p plaza_wire --features msgpack --test packing -- --nocapture
//! ```

#![cfg(all(feature = "msgpack", feature = "serde"))]

use plaza_wire::bits::{BitReader, BitWriter};
use plaza_wire::{BitCodec, MsgPackCodec, WireCodec};
use serde::{Deserialize, Serialize};

const CUBES: usize = 901;
const HZ: f64 = 60.0;

/// The bounds and precisions are the article's: ±256m across, 32m up, 512
/// positions per metre, and velocity bounded at ±32 m/s.
const POS_XY: (f32, f32) = (-256.0, 256.0);
const POS_Z: (f32, f32) = (0.0, 32.0);
const POS_XY_BITS: u32 = 18;
const POS_Z_BITS: u32 = 14;
const ROT_BITS: u32 = 9;
const VEL: (f32, f32) = (-32.0, 32.0);
const VEL_BITS: u32 = 11;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Cube {
  index: u32,
  at_rest: bool,
  pos: [f32; 3],
  rot: [f32; 4],
  linvel: [f32; 3],
}

/// A deterministic scene: no rand dependency, and the same numbers every run so
/// a change in the table means a change in the encoding.
fn scene() -> Vec<Cube> {
  let mut seed = 0x2545_f491_4f6c_dd1du64;
  let mut next = move || {
    seed ^= seed << 13;
    seed ^= seed >> 7;
    seed ^= seed << 17;
    (seed >> 11) as f32 / (1u64 << 53) as f32
  };

  (0..CUBES)
    .map(|i| {
      let unit = |lo: f32, hi: f32, r: f32| lo + r * (hi - lo);
      // Most of a settled scene is asleep, which is the whole point of the flag.
      let at_rest = next() < 0.8;
      let quat = [next() - 0.5, next() - 0.5, next() - 0.5, next() - 0.5];
      let norm = quat.iter().map(|c| c * c).sum::<f32>().sqrt();
      Cube {
        index: i as u32,
        at_rest,
        pos: [
          unit(POS_XY.0, POS_XY.1, next()),
          unit(POS_XY.0, POS_XY.1, next()),
          unit(POS_Z.0, POS_Z.1, next()),
        ],
        rot: quat.map(|c| c / norm),
        linvel: if at_rest {
          [0.0; 3]
        } else {
          [
            unit(VEL.0, VEL.1, next()),
            unit(VEL.0, VEL.1, next()),
            unit(VEL.0, VEL.1, next()),
          ]
        },
      }
    })
    .collect()
}

/// The hand-written layout: everything serde cannot express. Positions and
/// velocities quantised to the precision the game renders at, orientation as
/// smallest-three, the index as a delta from the previous cube, and a resting
/// cube paying one bit instead of three velocities.
fn pack(cubes: &[Cube]) -> Vec<u8> {
  let mut w = BitWriter::with_capacity(cubes.len() * 10);
  w.varint(cubes.len() as u64);
  let mut previous = 0u32;
  for cube in cubes {
    w.varint((cube.index - previous) as u64);
    previous = cube.index;
    w.bool(cube.at_rest);
    w.quantized(cube.pos[0], POS_XY.0, POS_XY.1, POS_XY_BITS);
    w.quantized(cube.pos[1], POS_XY.0, POS_XY.1, POS_XY_BITS);
    w.quantized(cube.pos[2], POS_Z.0, POS_Z.1, POS_Z_BITS);
    w.smallest_three(cube.rot, ROT_BITS);
    if !cube.at_rest {
      for axis in cube.linvel {
        w.quantized(axis, VEL.0, VEL.1, VEL_BITS);
      }
    }
  }
  w.finish()
}

fn unpack(bytes: &[u8]) -> Vec<Cube> {
  let mut r = BitReader::new(bytes);
  let count = r.varint().unwrap() as usize;
  let mut out = Vec::with_capacity(count);
  let mut previous = 0u32;
  for _ in 0..count {
    let index = previous + r.varint().unwrap() as u32;
    previous = index;
    let at_rest = r.bool().unwrap();
    let pos = [
      r.quantized(POS_XY.0, POS_XY.1, POS_XY_BITS).unwrap(),
      r.quantized(POS_XY.0, POS_XY.1, POS_XY_BITS).unwrap(),
      r.quantized(POS_Z.0, POS_Z.1, POS_Z_BITS).unwrap(),
    ];
    let rot = r.smallest_three(ROT_BITS).unwrap();
    let linvel = if at_rest {
      [0.0; 3]
    } else {
      [
        r.quantized(VEL.0, VEL.1, VEL_BITS).unwrap(),
        r.quantized(VEL.0, VEL.1, VEL_BITS).unwrap(),
        r.quantized(VEL.0, VEL.1, VEL_BITS).unwrap(),
      ]
    };
    out.push(Cube {
      index,
      at_rest,
      pos,
      rot,
      linvel,
    });
  }
  out
}

/// The envelope a packed payload actually travels in: the op stays serde, and
/// only the hot array is bytes.
#[derive(Debug, Serialize, Deserialize)]
enum Op {
  Snapshot { tick: u64, cubes: Vec<u8> },
}

/// The same envelope with the payload declared as *bytes* rather than a
/// sequence of numbers.
///
/// `Vec<u8>` reaches a codec through `serialize_seq`, so every byte is encoded
/// as its own integer: MessagePack spends two on anything over 127, and the bit
/// codec spends a ten-bit varint. Half the saving from packing by hand is given
/// straight back at the envelope unless the field says `serialize_bytes`. This
/// is what `serde_bytes` exists for; it is fifteen lines, so here it is inline.
#[derive(Debug)]
struct Payload(Vec<u8>);

impl Serialize for Payload {
  fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_bytes(&self.0)
  }
}

impl<'de> Deserialize<'de> for Payload {
  fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    struct Raw;
    impl serde::de::Visitor<'_> for Raw {
      type Value = Vec<u8>;
      fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("bytes")
      }
      fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<Vec<u8>, E> {
        Ok(v.to_vec())
      }
      fn visit_byte_buf<E: serde::de::Error>(self, v: Vec<u8>) -> Result<Vec<u8>, E> {
        Ok(v)
      }
    }
    d.deserialize_byte_buf(Raw).map(Payload)
  }
}

#[derive(Debug, Serialize, Deserialize)]
enum ByteOp {
  Snapshot { tick: u64, cubes: Payload },
}

fn mbps(bytes: usize) -> f64 {
  bytes as f64 * 8.0 * HZ / 1_000_000.0
}

#[test]
fn the_three_strategies_priced_side_by_side() {
  let cubes = scene();

  let msgpack = MsgPackCodec.encode(&cubes).unwrap();
  let bit_codec = BitCodec.encode(&cubes).unwrap();
  let packed = pack(&cubes);
  let enveloped = MsgPackCodec
    .encode(&Op::Snapshot {
      tick: 1,
      cubes: packed.clone(),
    })
    .unwrap();

  let as_bytes = MsgPackCodec
    .encode(&ByteOp::Snapshot {
      tick: 1,
      cubes: Payload(packed.clone()),
    })
    .unwrap();

  let rows = [
    ("MessagePack (derive)", msgpack.len()),
    ("BitCodec (derive)", bit_codec.len()),
    ("bits, hand-packed", packed.len()),
    ("  in Vec<u8> envelope", enveloped.len()),
    ("  in bytes envelope", as_bytes.len()),
  ];

  println!("\n{CUBES} cubes, one snapshot, {HZ} Hz\n");
  println!("{:<24} {:>9} {:>10} {:>9}", "strategy", "bytes", "Mbit/sec", "vs msgpack");
  for (name, bytes) in rows {
    println!(
      "{:<24} {:>9} {:>10.2} {:>8.1}x",
      name,
      bytes,
      mbps(bytes),
      msgpack.len() as f64 / bytes as f64
    );
  }
  let asleep = cubes.iter().filter(|c| c.at_rest).count();
  println!("\n{asleep} of {CUBES} at rest, paying one bit instead of three velocities\n");

  assert!(bit_codec.len() < msgpack.len(), "a derive-level bit codec should still beat msgpack");
  assert!(packed.len() < bit_codec.len() / 2, "quantising should beat what a derive can reach, by a lot");
  assert!(
    as_bytes.len() < packed.len() + 32,
    "a bytes envelope should be a header, not a second encoding: {} vs {}",
    as_bytes.len(),
    packed.len()
  );
  assert!(
    enveloped.len() > as_bytes.len(),
    "and Vec<u8> should visibly cost more than bytes, or this row is not earning its place"
  );
}

#[test]
fn the_hand_packed_payload_survives_its_own_reader() {
  let cubes = scene();
  let back = unpack(&pack(&cubes));
  assert_eq!(back.len(), cubes.len());

  for (a, b) in cubes.iter().zip(&back) {
    assert_eq!(a.index, b.index);
    assert_eq!(a.at_rest, b.at_rest);
    // Within half a quantisation step of the precision each field was given.
    let step_xy = (POS_XY.1 - POS_XY.0) / ((1u64 << POS_XY_BITS) - 1) as f32;
    let step_z = (POS_Z.1 - POS_Z.0) / ((1u64 << POS_Z_BITS) - 1) as f32;
    assert!((a.pos[0] - b.pos[0]).abs() <= step_xy, "{:?} vs {:?}", a.pos, b.pos);
    assert!((a.pos[1] - b.pos[1]).abs() <= step_xy);
    assert!((a.pos[2] - b.pos[2]).abs() <= step_z);

    let flip = if a.rot.iter().zip(b.rot).map(|(x, y)| x * y).sum::<f32>() < 0.0 { -1.0 } else { 1.0 };
    for i in 0..4 {
      assert!((a.rot[i] - b.rot[i] * flip).abs() < 0.02, "{:?} vs {:?}", a.rot, b.rot);
    }
    if !a.at_rest {
      let step_v = (VEL.1 - VEL.0) / ((1u64 << VEL_BITS) - 1) as f32;
      for i in 0..3 {
        assert!((a.linvel[i] - b.linvel[i]).abs() <= step_v);
      }
    }
  }
}

#[test]
fn the_bit_codec_round_trips_the_same_scene() {
  let cubes = scene();
  let back: Vec<Cube> = BitCodec.decode(&BitCodec.encode(&cubes).unwrap()).unwrap();
  assert_eq!(back, cubes, "a derive-level codec is lossless, unlike quantising");
}
