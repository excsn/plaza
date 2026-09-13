//! The wire version covers every type `GowOp` reaches.
//!
//! The types are resolved from the op instead of listed by file, because of
//! what poketo found: a file list covers the ops and not the types they carry,
//! so a payload one refactor away stops moving the version and two builds that
//! disagree about the wire complete the handshake before mis-decoding.

fn main() {
  plaza_wire::build::Wire::detect().emit();
}
