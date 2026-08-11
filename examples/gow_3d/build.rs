//! Only `protocol.rs` defines the wire. The zone stays on the server and
//! reaches a client through the projection in there.

fn main() {
  plaza_wire::build::emit(&["src/protocol.rs"]);
}
