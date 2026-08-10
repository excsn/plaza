//! Only `protocol.rs` defines the wire: the simulation's state is rapier's and
//! never crosses, so the projection in the protocol is the whole surface.

fn main() {
  plaza_wire::build::emit(&["src/protocol.rs"]);
}
