//! Only `protocol.rs` defines the wire: the world's own state stays on the
//! server, and both regimes cross through the projections in there.

fn main() {
  plaza_wire::build::emit(&["src/protocol.rs"]);
}
