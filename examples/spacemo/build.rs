//! Only `protocol.rs` defines the wire: the simulation's state stays on the
//! server and never crosses, so the protocol's projection is all that goes on
//! the wire.

fn main() {
  plaza_wire::build::emit(&["src/protocol.rs"]);
}
