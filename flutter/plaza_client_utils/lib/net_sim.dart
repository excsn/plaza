/// A deterministic network simulator for testing and demonstrating netcode.
///
/// Prediction, reconciliation and interpolation are only interesting under
/// latency, so exercising them means injecting delay, jitter and loss in a
/// *reproducible* way. [LatencyLink] is a one-way time-ordered delay queue, and
/// [Rng] is a tiny seeded PRNG so a run repeats exactly.
///
/// A separate entry point rather than part of `plaza_client_utils.dart`, matching
/// the Rust crate's `net-sim` feature gate: this is a test and demo aid, not part
/// of the client API, and an application should not pull it in by accident.
library;

export 'src/net_sim.dart' show LatencyLink, PacketOrdering, Rng;
