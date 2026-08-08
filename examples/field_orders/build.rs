//! Derives the wire format's version from the source that defines it, so the
//! server and the wasm client agree by construction when built from the same
//! code and disagree loudly when the bundle is stale. The mechanism lives in
//! `plaza_wire::build`; all this file decides is which sources define the wire.

fn main() {
  plaza_wire::build::emit(&["src/protocol.rs"]);
}
