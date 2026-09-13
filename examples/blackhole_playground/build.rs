//! Derives the wire format's version from the source that defines it.
//!
//! The server and the browser client are separate builds of the same crate, so
//! hashing the files that define the messages gives both the same number when
//! they are built from the same code and different numbers when the wasm bundle
//! is older than the server. The handshake in `sim::protocol` detects that
//! mismatch. Deriving the version means nobody has to remember to bump a
//! constant.
//!
//! The mechanism itself lives in `plaza_wire::build`, which documents what it
//! hashes and why it errs toward asking for a reload that was not strictly
//! needed. All this file decides is which sources define the wire.

fn main() {
  plaza_wire::build::emit(&["src/sim/protocol.rs", "src/sim/types.rs"]);
}
