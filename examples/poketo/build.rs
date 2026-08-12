//! `protocol.rs` names the ops, but the types they carry live elsewhere:
//! `Overworld` embeds `Trainer` and `BattleState` embeds `Battle`. Hashing the
//! ops alone lets a creature gain a field without the version moving, so two
//! builds that disagree about the wire complete the handshake and then
//! mis-decode.

fn main() {
  plaza_wire::build::emit(&["src/protocol.rs", "src/battle.rs", "src/grid.rs"]);
}
