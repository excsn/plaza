/// Where on the server timeline to render, kept a fixed delay behind the
/// estimated server clock.
///
/// The estimate free-runs on [advance] rather than snapping on every packet, so
/// the render target moves smoothly. [resync] and [observeRate] are the two ways
/// to correct it: one nudges the position, the other dilates the speed.
///
/// Milliseconds throughout. The Rust original is generic over its timestamp
/// type; Dart has no numeric trait bounds worth the ceremony, so this fixes the
/// unit rather than pretending to be generic over one.
///
/// Ported from `plaza_client_utils::interpolation::InterpolationClock`.
class InterpolationClock {
  InterpolationClock(int delayMs) : _delay = delayMs;

  int? _now;
  int _delay;
  double _rate = 1.0;

  /// Aligns the clock to server time. The first observation starts it; later
  /// ones are ignored, so the estimate free-runs.
  void observe(int serverTimeMs) => _now ??= serverTimeMs;

  /// Advances by one frame's worth of time. No effect before the first observe.
  void advance(int dtMs) {
    final now = _now;
    if (now != null) _now = now + dtMs;
  }

  /// The point on the server timeline to interpolate at: the estimate minus the
  /// delay, floored at zero. Null before the first observation.
  int? get target {
    final now = _now;
    if (now == null) return null;
    return now >= _delay ? now - _delay : 0;
  }

  bool get started => _now != null;

  int get delay => _delay;

  /// For a client that sizes its buffer dynamically: larger under jitter,
  /// smaller on a stable connection.
  set delay(int value) => _delay = value;

  /// Steers the estimate toward the newest server time seen, by [strength] in
  /// 0 to 1, so the target self-corrects as latency drifts instead of
  /// free-running. Call in place of [observe] on each packet.
  void resync(int newestServerTimeMs, double strength) {
    final s = strength.clamp(0.0, 1.0);
    final now = _now;
    if (now == null) {
      _now = newestServerTimeMs;
      return;
    }
    final corrected = now + (newestServerTimeMs - now) * s;
    _now = corrected < 0 ? 0 : corrected.toInt();
  }

  /// The rate-based cousin of [resync]: adjusts the estimate's *speed* so it
  /// glides into alignment rather than jumping. Pair with [advanceScaled].
  ///
  /// Behind the newest, run slightly fast; ahead of it, which means
  /// interpolation is starving, run slightly slow. The drift is normalised by
  /// the render delay, and [maxRateAdjust] bounds how far from real time it
  /// goes, keeping the speed change imperceptible while it converges.
  void observeRate(int newestServerTimeMs, double maxRateAdjust) {
    final max = maxRateAdjust.clamp(0.0, 1.0);
    final now = _now;
    if (now == null) {
      _now = newestServerTimeMs;
      _rate = 1.0;
      return;
    }
    final scale = _delay < 1 ? 1.0 : _delay.toDouble();
    final error = (newestServerTimeMs - now).toDouble();
    final normalised = (error / scale).clamp(-1.0, 1.0);
    _rate = 1.0 + max * normalised;
  }

  /// Advances by [dtMs] scaled by the current playback rate. Identical to
  /// [advance] while the rate is 1.
  void advanceScaled(int dtMs) {
    final now = _now;
    if (now != null) _now = now + (dtMs * _rate).round();
  }

  /// 1 is real time, above catches the estimate up, below lets the stream catch
  /// up. For a readout, or to spot a clock under sustained correction.
  double get playbackRate => _rate;

  /// Un-starts the clock, keeping the delay. The next observe seeds it again.
  ///
  /// Not in the Rust original, which rebuilds the value instead. Dart callers
  /// hold this behind a `final` field, and a resume needs the estimate thrown
  /// away without the holder being rebuilt around it.
  void reset() {
    _now = null;
    _rate = 1.0;
  }
}

/// One snapshot as the server declared it.
class ServerSnapshot<S> {
  const ServerSnapshot(this.timestampMs, this.state);
  final int timestampMs;
  final S state;
}

/// Holds recent snapshots and interpolates between the two that bracket a
/// render target.
///
/// The Rust original constrains its state type with an `Interpolatable` trait.
/// Dart takes the blend as a function instead, which is the same information
/// without a trait system to lean on.
///
/// Ported from `plaza_client_utils::interpolation::SnapshotBuffer`.
class SnapshotBuffer<S> {
  /// [lerp] blends two states by a factor in 0 to 1.
  ///
  /// Throws [ArgumentError] if [maxSize] is below 2: interpolation needs two
  /// snapshots to sit between.
  SnapshotBuffer({required this.maxSize, required this.lerp}) {
    if (maxSize < 2) {
      throw ArgumentError.value(maxSize, 'maxSize', 'must be at least 2 to interpolate');
    }
  }

  final int maxSize;
  final S Function(S a, S b, double t) lerp;
  final List<ServerSnapshot<S>> _snapshots = <ServerSnapshot<S>>[];

  /// Inserts in timestamp order, so a reordered packet still lands correctly.
  /// A duplicate timestamp replaces the earlier state.
  void add(int timestampMs, S state) {
    final snap = ServerSnapshot<S>(timestampMs, state);
    var i = _snapshots.length;
    while (i > 0 && _snapshots[i - 1].timestampMs > timestampMs) {
      i--;
    }
    if (i > 0 && _snapshots[i - 1].timestampMs == timestampMs) {
      _snapshots[i - 1] = snap;
    } else {
      _snapshots.insert(i, snap);
    }
    while (_snapshots.length > maxSize) {
      _snapshots.removeAt(0);
    }
  }

  /// The state at [targetMs].
  ///
  /// Between two snapshots it interpolates. Outside the buffer it clamps to the
  /// nearest end rather than extrapolating: extrapolation is a separate
  /// decision with its own failure mode, and silently doing it here would hide
  /// a starving stream.
  S? at(int targetMs) {
    if (_snapshots.isEmpty) return null;
    if (_snapshots.length == 1) return _snapshots.first.state;
    if (targetMs <= _snapshots.first.timestampMs) return _snapshots.first.state;
    if (targetMs >= _snapshots.last.timestampMs) return _snapshots.last.state;

    for (var i = 0; i < _snapshots.length - 1; i++) {
      final a = _snapshots[i];
      final b = _snapshots[i + 1];
      if (targetMs >= a.timestampMs && targetMs <= b.timestampMs) {
        final span = b.timestampMs - a.timestampMs;
        if (span <= 0) return b.state;
        return lerp(a.state, b.state, (targetMs - a.timestampMs) / span);
      }
    }
    return _snapshots.last.state;
  }

  int get length => _snapshots.length;
  bool get isEmpty => _snapshots.isEmpty;
  int? get newestTimestamp => _snapshots.isEmpty ? null : _snapshots.last.timestampMs;
  int? get oldestTimestamp => _snapshots.isEmpty ? null : _snapshots.first.timestampMs;
  void clear() => _snapshots.clear();
}
