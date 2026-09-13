//! The wire version covers every type `SkapeOp` reaches.
//!
//! The types are resolved from the op instead of listed by file. The world's
//! shape, its props and its pathfinder are outside it on purpose: none of them
//! is serialized, so moving a lake must not disconnect a client.

fn main() {
  plaza_wire::build::Wire::detect().emit();
}
