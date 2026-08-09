//! Derives the wire format's version by resolving types from the tagged roots.
//!
//! `LobbyOp` and `TableOp` carry the `plaza-wire: root` tag in `types.rs`;
//! everything they reach is hashed, plaza's own payload vocabulary rides in
//! through the baked-in constant, and a reference the resolver cannot place
//! fails this build by name.

fn main() {
  plaza_wire::build::Wire::detect()
    .dart("../../flutter/parlour_client/lib/wire_protocol.dart")
    .dart_types("../../flutter/parlour_client/lib/wire_types.dart")
    .emit();
}
