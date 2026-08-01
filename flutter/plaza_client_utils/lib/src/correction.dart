import 'dart:math' as math;

/// A correction, as the two states it moved between.
///
/// No distance metric is imposed: that would put a constraint on every user for
/// the benefit of the ones that want telemetry. The caller knows its own units,
/// so the subtraction is its business.
class Correction<S> {
  const Correction({required this.seen, required this.settled});

  /// Where the entity was being drawn before the correction landed.
  final S seen;

  /// The logical state after snapping to the server and replaying whatever it
  /// had not yet acknowledged. What the ease is now heading toward.
  final S settled;
}

/// A running picture of prediction error, and an adaptive test for what counts
/// as abnormal.
///
/// There is no fixed normal. A thirty-pixel correction is unremarkable at one
/// send rate and alarming at another, and the same holds across latency settings
/// and across how much contact the simulation is in. A constant threshold
/// reports whatever it was tuned against, so it goes quiet exactly when
/// conditions change and noisy for reasons unrelated to any bug.
///
/// So this tracks the mean and variance of what it is fed and flags a correction
/// that stands out from *those*, which keeps its meaning as conditions move.
///
/// Ported from `plaza_client_utils::correction::CorrectionMonitor`.
class CorrectionMonitor {
  /// Defaults chosen to be quiet in healthy play: a slow baseline, a four-sigma
  /// band, and a floor under it.
  CorrectionMonitor({
    double smoothing = 0.03,
    double sigma = 4.0,
    double floor = 0.0,
    this.warmup = 32,
  })  : _alpha = smoothing.clamp(0.0, 1.0),
        _sigma = sigma < 0 ? 0 : sigma,
        _floor = floor < 0 ? 0 : floor;

  final double _alpha;
  final double _sigma;
  final double _floor;

  /// How many samples to learn from before flagging anything. A monitor without
  /// one is loudest at the moment it knows least.
  final int warmup;

  double _mean = 0;
  double _var = 0;
  double _peak = 0;
  int _samples = 0;
  int _outliers = 0;

  bool get isWarmingUp => _samples < warmup;

  /// Folds a correction into the baseline and reports whether it was abnormal.
  ///
  /// The sample is clamped to the threshold before it updates the baseline.
  /// Without that, one respawn-sized correction lifts the mean and variance so
  /// far that genuine problems hide underneath for the next thousand packets.
  /// Clamping still lets a *sustained* shift move the baseline, which is what you
  /// want: a run that is simply harder to predict should re-centre what normal
  /// means rather than alarm for ever.
  bool record(double magnitude) {
    if (!magnitude.isFinite) return false;
    final m = magnitude < 0 ? 0.0 : magnitude;
    final warming = _samples < warmup;
    final abnormal = !warming && m > threshold;

    // Two things differ while warming up, and both matter. Nothing is flagged,
    // because a baseline starting at zero says every correction is enormous. And
    // the baseline is averaged exactly rather than exponentially, because an
    // exponential average approaches the truth from zero and would still be far
    // short of it when flagging began, so the first real samples would trip a
    // threshold built from a norm never reached.
    final alpha = warming ? math.max(_alpha, 1.0 / (_samples + 1)) : _alpha;
    final sample = warming ? m : math.min(m, threshold);

    final delta = sample - _mean;
    _mean += alpha * delta;
    _var += alpha * (delta * delta - _var);

    _samples++;
    if (m > _peak) _peak = m;
    if (abnormal) _outliers++;
    return abnormal;
  }

  /// Whether a magnitude would be abnormal, without recording it.
  bool isAbnormal(double magnitude) => !isWarmingUp && magnitude > threshold;

  /// The mean plus the sigma band.
  double get threshold => _mean + band;

  /// The band above the mean, never below the floor.
  ///
  /// The floor is worth setting. A spell of near-perfect prediction drives the
  /// variance toward zero, and without a floor the band collapses with it and
  /// every pixel of ordinary jitter reads as an outlier. It answers "how large a
  /// correction do I not care about, ever".
  double get band => math.max(_sigma * math.sqrt(math.max(_var, 0.0)), _floor);

  /// What "normal" currently means.
  double get norm => _mean;

  /// The largest correction ever recorded, unclamped.
  double get peak => _peak;

  /// Recorded, and how many were abnormal.
  (int, int) get counts => (_samples, _outliers);

  /// Forgets the baseline, keeping the tuning. For a new run or a deliberate
  /// discontinuity.
  void reset() {
    _mean = 0;
    _var = 0;
    _peak = 0;
    _samples = 0;
    _outliers = 0;
  }
}
