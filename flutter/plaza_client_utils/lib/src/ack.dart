/// How far back the mask reaches.
const int ackWindow = 64;

/// A newest sequence plus a bitmask of the ones before it, the shape every
/// reliable-over-unreliable protocol uses.
///
/// Ported from `plaza_client_utils::ack::AckWindow`.
class AckWindow {
  AckWindow();

  /// Rebuilds from a received pair.
  factory AckWindow.fromEncoded(int newest, int mask) {
    final w = AckWindow();
    w._newest = newest;
    w._mask = mask;
    w._started = true;
    return w;
  }

  int _newest = 0;
  int _mask = 0;
  bool _started = false;

  /// The pair to put on the wire, or null if nothing has arrived.
  (int, int)? encode() => _started ? (_newest, _mask) : null;

  /// Records an arrival, returning whether it was new. A duplicate, or one that
  /// has fallen out of the window, returns false.
  ///
  /// Handles reordering: a straggler arriving after a newer packet lands in its
  /// own slot rather than being taken for the new newest.
  bool observe(int seq) {
    if (!_started) {
      _started = true;
      _newest = seq;
      _mask = 0;
      return true;
    }
    if (seq > _newest) {
      final shift = seq - _newest;
      // A shift of exactly the window is the boundary worth care: every old bit
      // falls out, but the old newest lands in the last slot and must survive.
      // Using one threshold for both the shift and the too-far test loses it.
      final shifted = shift >= 64 ? 0 : _mask << shift;
      _mask = shift <= ackWindow ? shifted | (1 << (shift - 1)) : 0;
      _newest = seq;
      return true;
    }
    if (seq == _newest) return false;

    final back = _newest - seq;
    if (back > ackWindow) return false;
    final bit = 1 << (back - 1);
    final wasSet = (_mask & bit) != 0;
    _mask |= bit;
    return !wasSet;
  }

  int? get newest => _started ? _newest : null;

  /// Bit `i` is `newest - 1 - i`.
  int get mask => _mask;

  /// Anything outside the window is false, including sequences newer than the
  /// newest seen.
  bool contains(int seq) {
    if (!_started || seq > _newest) return false;
    if (seq == _newest) return true;
    final back = _newest - seq;
    return back <= ackWindow && (_mask & (1 << (back - 1))) != 0;
  }

  /// The gaps from [oldest] up to the newest, ascending: what a sender resends.
  ///
  /// Clamped to the window, so a peer that has fallen far behind asks for a
  /// bounded amount of work. Past the window the data is beyond recovery and the
  /// caller should be resynchronising, not backfilling.
  Iterable<int> missingSince(int oldest) sync* {
    if (!_started) return;
    var floor = _newest - ackWindow;
    if (floor < oldest) floor = oldest;
    for (var seq = floor; seq < _newest; seq++) {
      if (!contains(seq)) yield seq;
    }
  }

  /// The newest sequence such that **everything** from [first] up to it arrived.
  ///
  /// Not the same as [newest], and the difference is load-bearing. A protocol
  /// that retransmits wants the mask. A protocol that *re-derives* wants a state
  /// the peer provably reached, and receiving N+1 after losing N does not put a
  /// peer in the state N+1 implies: whatever N announced and N+1 had no reason
  /// to repeat is gone. Taking the newest set bit hands the sender a state that
  /// never existed, and the resulting divergence is permanent and close to
  /// invisible. Measured, it made loss recovery statistically indistinguishable
  /// from no recovery at every loss rate.
  ///
  /// Null when the run is empty, covering two cases a caller treats alike:
  /// [first] did not arrive, or it is older than the window can speak about.
  /// Neither is a reason to move the frontier backwards.
  int? contiguousBase(int first) {
    if (!contains(first)) return null;
    var base = first;
    while (base < _newest && contains(base + 1)) {
      base++;
    }
    return base;
  }

  /// How many slots are filled, the newest included.
  int get receivedInWindow {
    if (!_started) return 0;
    var bits = 0;
    var m = _mask;
    while (m != 0) {
      bits += m & 1;
      m = m >>> 1;
    }
    return 1 + bits;
  }

  void reset() {
    _newest = 0;
    _mask = 0;
    _started = false;
  }
}
