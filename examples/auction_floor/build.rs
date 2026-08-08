//! Derives the wire format's version from the source that defines it; the
//! mechanism and what it hashes live in `plaza_wire::build`.

fn main() {
  plaza_wire::build::emit(&["src/types.rs"]);
}
