//! The wire version covers every type `PoketoOp` reaches.
//!
//! The types are resolved from the op instead of listed by file. The file list
//! this replaced named `protocol.rs` alone but the ops carry types from two
//! other files: `Overworld` embeds `Trainer`, `BattleState` embeds `Battle`. A
//! creature could gain a field without the version moving, so two builds that
//! disagreed about the wire would complete the handshake and then mis-decode.
//! Walking the fields means nobody has to remember to update a list.

fn main() {
  plaza_wire::build::Wire::detect().emit();
}
