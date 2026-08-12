//! The wire is whatever `SkapeOp` reaches.
//!
//! Resolved rather than listed. The world's shape, its props and its
//! pathfinder are deliberately outside it: none of them is serialized, so
//! moving a lake must not disconnect a client.

fn main() {
  plaza_wire::build::Wire::detect().emit();
}
