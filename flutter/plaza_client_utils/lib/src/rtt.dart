import 'saturating.dart';

/// Smooths round-trip samples into a stable estimate.
///
/// Keeps an exponential moving average and the running minimum. The minimum
/// approximates true latency because jitter only ever adds delay, never
/// subtracts it.
///
/// Ported from `plaza_client_utils::rtt::RttEstimator`, which is authoritative.
class RttEstimator {
  /// [alpha] is the moving-average weight of each new sample, in `(0, 1]`.
  /// Smaller is steadier but slower to react.
  RttEstimator([double alpha = 0.1]) : _alpha = alpha.clamp(1e-7, 1.0);

  final double _alpha;
  double? _smoothedMs;
  double? _minMs;

  /// Smoothed mean deviation of the samples, RFC 6298 rttvar.
  double _varMs = 0;

  void observe(int rttSampleMs) {
    final sample = rttSampleMs.toDouble();
    final prev = _smoothedMs;
    if (prev != null) {
      // Deviation updates against the old average before the average moves, as
      // in RFC 6298, and moves a little faster than the mean.
      final beta = (_alpha * 2.0).clamp(0.0, 1.0);
      _varMs += ((prev - sample).abs() - _varMs) * beta;
      _smoothedMs = prev + (sample - prev) * _alpha;
    } else {
      _smoothedMs = sample;
      _varMs = sample / 2.0;
    }
    final min = _minMs;
    _minMs = min == null ? sample : (sample < min ? sample : min);
  }

  /// The round trip is [nowMs] minus the origin time the ping carried.
  ///
  /// Saturating, so a reply stamped after its own arrival reads as zero rather
  /// than as a negative round trip that then poisons the average.
  void observePong(int originTimeMs, int nowMs) => observe(saturatingSub(nowMs, originTimeMs));

  double? get rttMs => _smoothedMs;

  double? get oneWayMs => _smoothedMs == null ? null : _smoothedMs! / 2.0;

  double? get minRttMs => _minMs;

  /// Smoothed mean deviation of the round-trip samples. Size a dynamic
  /// interpolation buffer from this, larger when the connection is unstable.
  double? get jitterMs => _smoothedMs == null ? null : _varMs;

  void clear() {
    _smoothedMs = null;
    _minMs = null;
    _varMs = 0;
  }
}
