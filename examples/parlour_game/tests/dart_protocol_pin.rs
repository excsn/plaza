//! The committed Dart copy of the protocol version is this build's.
//!
//! `build.rs` rewrites the file whenever the wire sources change, so this can
//! only fail when someone commits a wire change without building the server,
//! which is exactly the drift the handshake exists to catch.

use plaza_example_parlour_game::types::PROTOCOL;

#[test]
fn the_dart_client_carries_this_builds_protocol() {
  let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../flutter/parlour_client/lib/wire_protocol.dart");
  let dart = std::fs::read_to_string(path).expect("the generated wire_protocol.dart is committed");
  assert!(
    dart.contains(&format!("const int wireProtocol = {PROTOCOL};")),
    "the Dart client would announce a different protocol than this build speaks; rebuild plaza_example_parlour_game to refresh {path}"
  );
}
