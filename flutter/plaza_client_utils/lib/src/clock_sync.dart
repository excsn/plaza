import 'dart:collection';

/// A single clock measurement.
class _Sample {
  const _Sample(this.localMs, this.offsetMs);
  final double localMs;
  final double offsetMs;
}

/// Fits the client-to-server clock offset and skew by least squares over a
/// sliding window.
///
/// Offset alone treats the server clock as a fixed distance away. Real clocks
/// run at slightly different rates, so over a long session the true offset
/// ramps, and a fitted line tracks that ramp where an average lags it.
///
/// A round trip measures total delay, not each leg, so where the network is
/// asymmetric the one-way offset is unrecoverable from RTT alone. Regression
/// recovers the drift rate cleanly; it does not recover the asymmetric
/// constant. Size the interpolation buffer to absorb the residual.
///
/// Ported from `plaza_client_utils::clock_sync::ClockSyncEstimator`, which is
/// authoritative. Dart `double` is IEEE 754 binary64, matching the Rust `f64`.
class ClockSyncEstimator {
  /// Fits over at most [window] recent measurements. Larger is steadier but
  /// slower to follow a genuine change; 16 to 64 is typical.
  ///
  /// Throws [ArgumentError] if [window] is below 2, since a line needs two
  /// points.
  ClockSyncEstimator(int window)
      : _capacity = window,
        _window = Queue<_Sample>() {
    if (window < 2) {
      throw ArgumentError.value(window, 'window', 'must be at least 2');
    }
  }

  final Queue<_Sample> _window;
  final int _capacity;

  /// `offsetMs` is `serverTime - localTime`, observed when the local clock read
  /// [localMs]. The oldest sample drops when the window is full.
  void observe(double localMs, double offsetMs) {
    if (_window.length == _capacity) _window.removeFirst();
    _window.addLast(_Sample(localMs, offsetMs));
  }

  /// Derives the offset from a symmetric round-trip exchange, taken at
  /// [localRecv]. Where delay is asymmetric this offset carries that error.
  void observeExchange(double localSend, double serverRecv, double localRecv) {
    observe(localRecv, serverRecv - (localSend + localRecv) / 2.0);
  }

  bool get isReady => _window.length >= 2;

  int get sampleCount => _window.length;

  /// `(meanLocal, meanOffset, skew)`, centred on the window mean for numerical
  /// stability. Null with fewer than two samples.
  List<double>? _fit() {
    final n = _window.length;
    if (n < 2) return null;
    final nf = n.toDouble();
    var meanX = 0.0;
    var meanY = 0.0;
    for (final s in _window) {
      meanX += s.localMs;
      meanY += s.offsetMs;
    }
    meanX /= nf;
    meanY /= nf;

    var sxy = 0.0;
    var sxx = 0.0;
    for (final s in _window) {
      final dx = s.localMs - meanX;
      sxy += dx * (s.offsetMs - meanY);
      sxx += dx * dx;
    }
    // Degenerate x-spread, every sample at one instant: no slope, flat offset.
    final skew = sxx > 1e-9 ? sxy / sxx : 0.0;
    return <double>[meanX, meanY, skew];
  }

  /// The estimated offset at [localMs] along the fitted line, so it interpolates
  /// within the window and extrapolates past it. With one sample, that sample's
  /// offset.
  double? offsetAt(double localMs) {
    final f = _fit();
    if (f == null) return _window.isEmpty ? null : _window.last.offsetMs;
    return f[1] + f[2] * (localMs - f[0]);
  }

  /// [localMs] plus the fitted offset.
  double? serverTimeAt(double localMs) {
    final off = offsetAt(localMs);
    return off == null ? null : localMs + off;
  }

  /// How fast the offset changes per unit of local time. Multiply by 1e6 for
  /// parts per million. Zero until a line can be fit.
  double get skew => _fit()?[2] ?? 0.0;

  void clear() => _window.clear();
}
