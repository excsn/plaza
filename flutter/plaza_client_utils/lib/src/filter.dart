/// A one-dimensional Kalman filter over a scalar signal.
///
/// [RttEstimator] smooths with a fixed-weight moving average: cheap, tuning-free
/// and the right default. A moving average trusts every sample equally for ever,
/// though. This tracks how *confident* it is and weights each measurement
/// against that, so it settles quickly then rejects jitter once settled.
///
/// Two knobs, and they are the point of a building block:
///
/// - **process noise** (Q): how much the true value is expected to wander
///   between samples. Higher trusts new measurements more, faster and jumpier.
/// - **measurement noise** (R): how noisy each reading is. Higher smooths
///   harder, slower and steadier.
///
/// Ported from `plaza_client_utils::filter::ScalarKalman`.
class ScalarKalman {
  /// The first [observe] seeds the estimate, so no initial value is needed.
  /// [measurementNoise] is floored just above zero to keep the gain finite.
  ScalarKalman(double processNoise, double measurementNoise)
      : _processNoise = processNoise < 0 ? 0 : processNoise,
        _measurementNoise = measurementNoise < 1e-9 ? 1e-9 : measurementNoise;

  /// Seeds explicitly, with an initial variance. Smaller means more trusted.
  factory ScalarKalman.seeded(
    double processNoise,
    double measurementNoise, {
    required double estimate,
    required double variance,
  }) {
    final k = ScalarKalman(processNoise, measurementNoise);
    k._estimate = estimate;
    k._variance = variance < 0 ? 0 : variance;
    k._initialized = true;
    return k;
  }

  double _processNoise;
  double _measurementNoise;
  double _estimate = 0;
  double _variance = 0;
  double _lastGain = 0;
  bool _initialized = false;

  /// Folds in a measurement and returns the updated estimate.
  ///
  /// The first call takes the measurement as the estimate. After that: predict,
  /// so variance grows by the process noise, then correct toward the measurement
  /// by the gain, which is large while uncertain and shrinks as it settles.
  double observe(double measurement) {
    if (!_initialized) {
      _estimate = measurement;
      _variance = _measurementNoise;
      _initialized = true;
      _lastGain = 1.0;
      return _estimate;
    }
    _variance += _processNoise;
    final gain = _variance / (_variance + _measurementNoise);
    _estimate += gain * (measurement - _estimate);
    _variance *= 1.0 - gain;
    _lastGain = gain;
    return _estimate;
  }

  double get estimate => _estimate;

  /// How uncertain the filter is. Shrinks as it settles, grows under process noise.
  double get variance => _variance;

  /// The gain used on the last measurement, 0 to 1: near 1 while settling and
  /// trusting measurements, near 0 once settled and rejecting jitter.
  double get lastGain => _lastGain;

  bool get isInitialized => _initialized;

  set processNoise(double q) => _processNoise = q < 0 ? 0 : q;

  set measurementNoise(double r) => _measurementNoise = r < 1e-9 ? 1e-9 : r;

  /// Forgets everything; the next measurement re-seeds.
  void reset() {
    _initialized = false;
    _estimate = 0;
    _variance = 0;
    _lastGain = 0;
  }
}
