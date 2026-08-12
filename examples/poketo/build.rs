//! The wire is whatever `PoketoOp` reaches.
//!
//! Resolved rather than listed. The file list this replaced named
//! `protocol.rs` alone, and the ops carry types from two other files:
//! `Overworld` embeds `Trainer`, `BattleState` embeds `Battle`. A creature
//! could gain a field without the version moving, so two builds that disagreed
//! about the wire would complete the handshake and then mis-decode. Walking
//! the fields is the only version of this a person cannot forget to update.

fn main() {
  plaza_wire::build::Wire::detect().emit();
}
