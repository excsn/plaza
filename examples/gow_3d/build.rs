//! The wire is whatever `GowOp` reaches.
//!
//! Resolved rather than listed, for the reason poketo found out the hard way:
//! a file list covers the ops and not the types they carry, so a payload one
//! refactor away stops moving the version and two builds that disagree about
//! the wire complete the handshake before mis-decoding.

fn main() {
  plaza_wire::build::Wire::detect().emit();
}
