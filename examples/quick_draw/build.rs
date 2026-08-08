//! Derives the wire format's version from the source that defines it; see
//! `plaza_wire::build` for what is hashed and why.

fn main() {
  plaza_wire::build::emit(&["src/protocol.rs"]);
}
