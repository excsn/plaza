//! Derives the wire format's version from the source that defines it, so the
//! server and the wasm client agree when built from the same code and report a
//! mismatch when the bundle is stale. The mechanism lives in
//! `plaza_wire::build`; all this file decides is which sources define the wire.

fn main() {
  plaza_wire::build::emit(&["src/protocol.rs"]);
}
