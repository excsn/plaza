import 'dart:collection';

/// A tiny deterministic PRNG (xorshift64). Not for anything but reproducible
/// jitter and loss.
///
/// `>>>` rather than `>>`, and the same reason as `mix64`: Dart's `>>`
/// sign-extends and this is a u64 algorithm. The sequence agrees with the Rust
/// `Rng` for the same seed, which is what makes a Rust scenario and a Dart one
/// comparable at all.
///
/// Ported from `plaza_client_utils::net_sim::Rng`.
class Rng {
  /// A fixed seed makes a run repeatable.
  Rng(int seed) : _state = seed | 1;

  int _state;

  int _nextU64() {
    var x = _state;
    x ^= x << 13;
    x ^= x >>> 7;
    x ^= x << 17;
    _state = x;
    return x;
  }

  /// A double in `[0, 1)`.
  ///
  /// Drawn from the top 24 bits, so the quotient is exact in both `f32` and
  /// `double` and agrees with Rust exactly rather than approximately.
  double unit() => (_nextU64() >>> 40) / 16777216.0;

  /// An integer in `[0, n]` inclusive.
  int upTo(int n) {
    if (n <= 0) return 0;
    return _unsignedMod(_nextU64(), n + 1);
  }

  /// `x` as a u64, modulo `m`.
  ///
  /// Dart's `%` on a negative left operand is the mathematical modulo of a
  /// *signed* value, and half of every PRNG's draws have the top bit set. So the
  /// negative case is folded back by hand: `u = 2^63 + r`, and `2^63 mod m` is
  /// accumulated by doubling, never exceeding `m`.
  ///
  /// Requires `m <= 2^62`, which every jitter value is by a wide margin.
  static int _unsignedMod(int x, int m) {
    if (x >= 0) return x % m;
    final r = x & 0x7FFFFFFFFFFFFFFF;
    var p = 1;
    for (var i = 0; i < 63; i++) {
      p = (p * 2) % m;
    }
    return (p + r % m) % m;
  }
}

/// Whether the simulated wire may deliver packets out of send order.
///
/// This is not a detail. Impairment tooling that can produce failures the real
/// transport cannot is worse than no tooling, because the failures are credible
/// enough to spend a day chasing, and because it quietly hides that the real
/// system has stronger guarantees than the tests assume. Pick the one that matches
/// the transport being stood in for.
enum PacketOrdering {
  /// Jitter delays a packet, possibly past its successors, but never ahead of its
  /// predecessors. **The default**, and what TCP, WebSocket, QUIC streams and any
  /// ordered channel actually do.
  ordered,

  /// Jitter may reorder freely, so a later packet can arrive first. What raw UDP
  /// and unordered datagram channels do. Choose this deliberately: a delta stream
  /// that assumes ordering will diverge under it, which is a real finding on a
  /// datagram transport and a phantom on an ordered one.
  unordered,
}

/// One direction of a simulated wire: packets handed in at `now` become
/// deliverable at `now + latency (+ jitter)`, unless dropped. Generic over the
/// packet type, so drive both directions with two of them.
///
/// Ported from `plaza_client_utils::net_sim::LatencyLink`.
class LatencyLink<T> {
  LatencyLink({this.ordering = PacketOrdering.ordered});

  /// Whether jitter may reorder. See [PacketOrdering].
  final PacketOrdering ordering;

  final Queue<(int, T)> _queue = Queue<(int, T)>();

  /// The delivery time of the last packet queued, so an ordered link can hold a
  /// jittered packet back behind its predecessor.
  int _lastDeliverMs = 0;

  /// Hands a packet to the wire. It may be delayed by [latencyMs] plus up to
  /// [jitterMs], or dropped with probability `lossPct / 100`.
  ///
  /// Under [PacketOrdering.ordered] the jittered delivery time is clamped to at
  /// least the previous packet's, so jitter shows up as a packet arriving late
  /// rather than as the stream shuffling. Loss is independent of ordering: an
  /// ordered transport still loses whole connections and, at this level of
  /// abstraction, still models a dropped application message.
  void send(
    int nowMs,
    T packet, {
    required int latencyMs,
    int jitterMs = 0,
    double lossPct = 0.0,
    required Rng rng,
  }) {
    if (lossPct > 0.0 && rng.unit() * 100.0 < lossPct) return;
    var deliverAt = nowMs + latencyMs + rng.upTo(jitterMs);
    if (ordering == PacketOrdering.ordered && deliverAt < _lastDeliverMs) {
      deliverAt = _lastDeliverMs;
    }
    _lastDeliverMs = deliverAt;
    _queue.addLast((deliverAt, packet));
  }

  /// Removes and returns every packet whose delivery time has arrived, oldest
  /// delivery first.
  List<T> drainDue(int nowMs) {
    final due = <(int, T)>[];
    final kept = Queue<(int, T)>();
    for (final entry in _queue) {
      if (entry.$1 <= nowMs) {
        due.add(entry);
      } else {
        kept.addLast(entry);
      }
    }
    _queue
      ..clear()
      ..addAll(kept);
    // Stable, so packets sharing a delivery time keep their send order, which is
    // what an ordered link promises after clamping.
    mergeSortByDeliveryTime(due);
    return due.map((e) => e.$2).toList(growable: false);
  }

  /// Enqueues a packet at an exact delivery time, bypassing latency, jitter and
  /// loss. For tests that need a specific arrival order.
  void enqueueAt(int deliverAtMs, T packet) {
    _lastDeliverMs = deliverAtMs > _lastDeliverMs ? deliverAtMs : _lastDeliverMs;
    _queue.addLast((deliverAtMs, packet));
  }

  int get inFlight => _queue.length;
}

/// A stable sort by delivery time.
///
/// Dart's `List.sort` is not stable, and an unstable sort here would reorder
/// packets that share a delivery time, which is exactly the shuffling
/// [PacketOrdering.ordered] exists to rule out.
void mergeSortByDeliveryTime<T>(List<(int, T)> items) {
  if (items.length < 2) return;
  final buffer = List<(int, T)>.of(items);
  void merge(int lo, int mid, int hi) {
    var i = lo;
    var j = mid;
    for (var k = lo; k < hi; k++) {
      if (i < mid && (j >= hi || buffer[i].$1 <= buffer[j].$1)) {
        items[k] = buffer[i++];
      } else {
        items[k] = buffer[j++];
      }
    }
  }

  void sort(int lo, int hi) {
    if (hi - lo < 2) return;
    final mid = lo + (hi - lo) ~/ 2;
    sort(lo, mid);
    sort(mid, hi);
    buffer.setRange(lo, hi, items, lo);
    merge(lo, mid, hi);
  }

  sort(0, items.length);
}
