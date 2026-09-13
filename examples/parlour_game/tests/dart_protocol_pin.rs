//! The committed Dart copy of the protocol version is this build's.
//!
//! `build.rs` rewrites the file whenever the wire changes, so this can only
//! fail when someone commits a wire change without building the server, which
//! is the drift the handshake catches.

use plaza_example_parlour_game::types::PROTOCOL;

#[test]
fn the_dart_client_carries_this_builds_protocol() {
  plaza_wire::build::assert_dart_protocol(
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../flutter/parlour_client/lib/wire_protocol.dart"),
    PROTOCOL,
  );
}
