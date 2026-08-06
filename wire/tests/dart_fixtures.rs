//! Golden wire vectors, so the Dart mirror can be checked against this crate
//! rather than against someone's reading of it.
//!
//! Each fixture is written twice: `.msgpack` is what a client actually receives,
//! and `.json` is the same value in a form a human can read and a Dart test can
//! compare against without trusting its own MessagePack decoder to bootstrap
//! itself.
//!
//! The committed files are asserted to match, so a change to the wire format
//! fails here rather than silently in a Dart app. Regenerate deliberately:
//!
//! ```sh
//! PLAZA_REGENERATE_FIXTURES=1 cargo test -p plaza_wire --features msgpack,json --test dart_fixtures
//! ```

#![cfg(all(feature = "msgpack", feature = "json"))]

use std::fs;
use std::path::PathBuf;

use plaza_wire::frame::ProtocolVersion;
use plaza_wire::{frame, MsgPackCodec, MsgPackNamedCodec, WireCodec};
use serde::{Deserialize, Serialize};

/// One of each variant shape serde produces, because the shapes are what a
/// hand-written client gets wrong: a unit variant is a bare string and every
/// other is a one-entry map, and a client that only checks for a property drops
/// the first kind entirely.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum FixtureOp {
  /// Unit: encodes as the bare string "Ping".
  Ping,
  /// Struct: `{"Move": {"x": .., "y": ..}}`.
  Move { x: i32, y: i32 },
  /// Newtype: `{"Say": "..."}`.
  Say(String),
  /// Tuple: `{"Pair": [.., ..]}`.
  Pair(u8, u8),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Edges {
  zero: i64,
  fixint_max: i64,
  u8_edge: i64,
  u16_edge: i64,
  u32_edge: i64,
  neg_fixint: i64,
  i8_edge: i64,
  i16_edge: i64,
  i32_edge: i64,
  float: f64,
  yes: bool,
  no: bool,
  nothing: Option<u8>,
  empty_str: String,
  fixstr_max: String,
  str8: String,
  unicode: String,
  empty_list: Vec<u8>,
}

fn dir() -> PathBuf {
  PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../flutter/fixtures")
}

/// Writes when regenerating, asserts otherwise.
fn golden(name: &str, ext: &str, bytes: &[u8]) {
  let path = dir().join(format!("{name}.{ext}"));
  if std::env::var("PLAZA_REGENERATE_FIXTURES").is_ok() {
    fs::create_dir_all(dir()).expect("fixture dir");
    fs::write(&path, bytes).expect("write fixture");
    return;
  }
  let committed = fs::read(&path).unwrap_or_else(|e| {
    panic!("missing fixture {}: {e}. Regenerate with PLAZA_REGENERATE_FIXTURES=1", path.display())
  });
  assert_eq!(
    committed,
    bytes,
    "fixture {} is stale; the wire format changed. Regenerate deliberately, and update the Dart side.",
    path.display()
  );
}

fn emit<T: Serialize>(name: &str, value: &T) {
  golden(name, "msgpack", &MsgPackCodec.encode(value).expect("msgpack"));
  golden(name, "named.msgpack", &MsgPackNamedCodec.encode(value).expect("msgpack-named"));
  golden(name, "json", &serde_json::to_vec(value).expect("json"));
}

/// Digest values, so the Dart port is checked against this arithmetic rather
/// than against a reading of it.
///
/// A digest that disagrees is the worst failure available: both sides would
/// blame the world for a bug that was only ever in the hashing, and the
/// recovery machinery would fire for ever.
#[test]
fn set_digest_values_are_pinned() {
  use plaza_client_utils::SetDigest;
  let cases: Vec<(&str, Vec<u64>)> = vec![
    ("empty", vec![]),
    ("one", vec![1]),
    ("zero", vec![0]),
    ("small", vec![1, 2, 3]),
    ("reordered", vec![3, 1, 2]),
    ("duplicate", vec![1, 1]),
    ("dense", (0..64).collect()),
    ("sparse", vec![1, 1_000, 1_000_000, u32::MAX as u64]),
    ("high_bits", vec![u64::MAX, u64::MAX - 1, 1 << 63]),
  ];
  let mut out = String::new();
  for (name, keys) in &cases {
    let d = SetDigest::from_keys(keys.iter().copied());
    out.push_str(&format!("{name} {} {}\n", d.len(), d.digest() as i64));
  }
  golden("digests", "txt", out.as_bytes());
}

#[test]
fn ops_batch_covers_every_variant_shape() {
  let ops = vec![
    FixtureOp::Ping,
    FixtureOp::Move { x: -7, y: 300 },
    FixtureOp::Say("hello".to_string()),
    FixtureOp::Pair(1, 2),
  ];
  emit("ops_batch", &ops);
}

#[test]
fn an_empty_batch_is_still_a_list() {
  emit("ops_empty", &Vec::<FixtureOp>::new());
}

#[test]
fn edge_encodings_for_every_width() {
  emit("edges", &Edges {
    zero: 0,
    fixint_max: 127,
    u8_edge: 255,
    u16_edge: 65535,
    u32_edge: 4294967295,
    neg_fixint: -32,
    i8_edge: -128,
    i16_edge: -32768,
    i32_edge: -2147483648,
    float: 3.141592653589793,
    yes: true,
    no: false,
    nothing: None,
    empty_str: String::new(),
    fixstr_max: "x".repeat(31),
    str8: "y".repeat(32),
    unicode: "héllo 世界 🎲".to_string(),
    empty_list: Vec::new(),
  });
}

#[test]
fn the_hello_body_is_a_bare_number() {
  emit("hello", &ProtocolVersion(0xDEAD_BEEF).0);
}

/// The whole frame, tag byte included, so the Dart side can check its splitter
/// against a real one rather than one it built itself.
#[test]
fn whole_frames_carry_their_tag() {
  let mut buf = Vec::new();
  frame::begin(frame::Kind::Ops, &mut buf);
  MsgPackCodec
    .encode_into(&vec![FixtureOp::Ping, FixtureOp::Pair(3, 4)], &mut buf)
    .expect("encode");
  golden("frame_ops", "bin", &buf);

  let mut buf = Vec::new();
  frame::begin(frame::Kind::Hello, &mut buf);
  MsgPackCodec.encode_into(&7u32, &mut buf).expect("encode");
  golden("frame_hello", "bin", &buf);

  let mut buf = Vec::new();
  frame::begin(frame::Kind::Ping, &mut buf);
  MsgPackCodec
    .encode_into(&frame::Ping { origin: 1_234_567 }, &mut buf)
    .expect("encode");
  golden("frame_ping", "bin", &buf);

  // Both shapes of the reply, because the absent clock is the one a port is
  // likely to encode as zero and so lose.
  let mut buf = Vec::new();
  frame::begin(frame::Kind::Pong, &mut buf);
  MsgPackCodec
    .encode_into(
      &frame::Pong {
        origin: 1_234_567,
        responder: Some(89),
      },
      &mut buf,
    )
    .expect("encode");
  golden("frame_pong", "bin", &buf);

  let mut buf = Vec::new();
  frame::begin(frame::Kind::Pong, &mut buf);
  MsgPackCodec
    .encode_into(
      &frame::Pong {
        origin: 1_234_567,
        responder: None,
      },
      &mut buf,
    )
    .expect("encode");
  golden("frame_pong_no_clock", "bin", &buf);
}
