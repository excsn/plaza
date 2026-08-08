//! Derives the wire format's version from the sources that define it; the
//! simulation is on the wire (whole worlds and applied inputs), so it counts.

fn main() {
  plaza_wire::build::emit(&["src/protocol.rs", "src/sim.rs"]);
}
