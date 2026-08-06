//! Derives the wire format's version from the source that defines it.
//!
//! The mechanism lives in `plaza_wire::build`, which documents what it hashes and
//! why; all this decides is which source defines the wire. Here that is one file,
//! because the ops and everything they carry live in `types.rs`.

fn main() {
  plaza_wire::build::emit(&["src/types.rs"]);
}
