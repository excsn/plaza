//! Derives the wire format's version from the source that defines it.
//!
//! The server and the browser client are separate builds of the same crate, so
//! hashing the files that define the messages gives both the same number when
//! they are built from the same code and different numbers when the wasm bundle
//! is older than the server. The mechanism lives in `plaza_wire::build`, which
//! documents what it hashes and why; all this decides is which sources define
//! the wire.

fn main() {
  // **`rules.rs` is included on purpose.** The recorded input logs are replayed
  // through these rules, so the rules are part of the contract just like the
  // message shapes: a ghost recorded before a handling change drives
  // differently and including the rules makes that detectable.
  plaza_wire::build::emit(&["src/sim/protocol.rs", "src/sim/types.rs", "src/sim/rules.rs"]);
}
