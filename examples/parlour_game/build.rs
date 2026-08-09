//! Derives the wire format's version from the sources that define it.
//!
//! The mechanism lives in `plaza_wire::build`, which documents what it hashes
//! and why; all this decides is which sources define the wire. The ops live in
//! `types.rs`, but the notice payloads they carry are defined in plaza core,
//! and the hash reads text without resolving types, so those files are listed
//! too or a payload shape change would not move the version. The managers
//! sharing those files over-bump it, which is the direction the mechanism is
//! documented to err in.

fn main() {
  let sources = [
    "src/types.rs",
    "../../core/src/game_common/flow_control/phases.rs",
    "../../core/src/game_common/flow_control/turns.rs",
    "../../core/src/game_common/flow_control/rounds.rs",
  ];
  plaza_wire::build::emit(&sources);
  plaza_wire::build::emit_dart(&sources, "../../flutter/parlour_client/lib/wire_protocol.dart");
}
