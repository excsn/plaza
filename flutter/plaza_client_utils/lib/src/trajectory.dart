/// Second-order dead reckoning: coasting a remote entity through a gap in the
/// packet stream using where it was *heading*, not just how fast it was going.
///
/// `ExtrapolationBase` coasts on the velocity a snapshot carried, which is first
/// order and therefore exactly wrong for anything turning: a target on a curve is
/// projected straight off the tangent, and the longer the gap the further off it
/// flies. Most things worth extrapolating are turning.
///
/// [TrajectoryPredictor] fits the next order up. It keeps the last three samples,
/// takes velocity from the newest pair and acceleration from the change between
/// pairs, and projects a curve. That is strictly better over short gaps and
/// strictly worse over long ones, because a quadratic diverges faster than a line,
/// so the acceleration term is **damped** by a coefficient and the whole
/// projection is clamped to a horizon. Both are the caller's to set, and the
/// defaults are deliberately timid.
///
/// Scalar on purpose, matching `ScalarKalman`: run one per axis. A
/// generic-over-state version would need a vector-space bound that every consumer
/// would then have to satisfy, for arithmetic the consumer can do in two lines.
///
/// ```dart
/// // A value accelerating: 0, 1, 4 at 100ms apart.
/// final p = TrajectoryPredictor(damping: 1.0, maxHorizonMs: 500)
///   ..observe(0, 0.0)
///   ..observe(100, 1.0)
///   ..observe(200, 4.0);
///
/// // A straight line off the last two samples would say 7. The curve says 8.
/// assert(p.predict(300)! > 7.5);
/// ```
///
/// Ported from `plaza_client_utils::trajectory::TrajectoryPredictor`.
class TrajectoryPredictor {
  /// [damping] scales the acceleration term: `0.0` is plain constant-velocity dead
  /// reckoning, `1.0` is the full quadratic. Values around `0.5` are the usual
  /// choice, because a fitted acceleration is the noisiest thing three samples can
  /// tell you and trusting it fully turns measurement noise into visible overshoot.
  ///
  /// [maxHorizonMs] clamps how far past the newest sample a prediction may reach.
  /// Beyond it the projection is evaluated *at* the horizon and held, which stops a
  /// lost stream from flinging an entity off the map. There is no safe unbounded
  /// setting, which is why it is a constructor argument rather than an option.
  TrajectoryPredictor({required double damping, required this.maxHorizonMs})
      : damping = damping.clamp(0.0, 1.0);

  final double damping;
  final int maxHorizonMs;

  /// Newest last. [samples] says how many are valid.
  final List<int> _times = [0, 0, 0];
  final List<double> _values = [0.0, 0.0, 0.0];
  int _count = 0;

  /// Records a sample. Samples at or before the newest are ignored: a straggler
  /// arriving out of order would otherwise invert the fitted derivatives and send
  /// the prediction backwards.
  void observe(int timeMs, double value) {
    if (_count > 0 && timeMs <= _times[_count - 1]) return;
    if (_count == 3) {
      _times[0] = _times[1];
      _times[1] = _times[2];
      _times[2] = timeMs;
      _values[0] = _values[1];
      _values[1] = _values[2];
      _values[2] = value;
    } else {
      _times[_count] = timeMs;
      _values[_count] = value;
      _count++;
    }
  }

  /// The value projected to [timeMs].
  ///
  /// Null until a sample has arrived. With one sample it holds that value; with
  /// two it is first order; with three it is the damped curve. Degrading by sample
  /// count rather than refusing to answer is what lets a caller use it from the
  /// first packet.
  ///
  /// Times before the newest sample are answered by the same polynomial, so this
  /// interpolates as readily as it extrapolates.
  double? predict(int timeMs) {
    if (_count == 0) return null;
    final newest = _times[_count - 1];
    final base = _values[_count - 1];

    // Clamp forward only. Extrapolation is what runs away; going back through the
    // fitted samples is bounded by the samples themselves.
    final horizon = newest + maxHorizonMs;
    final target = timeMs < horizon ? timeMs : horizon;
    final dt = (target - newest) / 1000.0;

    final v = velocity ?? 0.0;
    final a = (acceleration ?? 0.0) * damping;
    return base + v * dt + 0.5 * a * dt * dt;
  }

  /// Rate of change from the newest pair, per second. Null with fewer than two
  /// samples.
  double? get velocity {
    if (_count < 2) return null;
    final i = _count - 2;
    final j = _count - 1;
    final dt = (_times[j] - _times[i]) / 1000.0;
    if (dt <= 0.0) return null;
    return (_values[j] - _values[i]) / dt;
  }

  /// Change in rate across the two most recent intervals, per second squared. Null
  /// with fewer than three samples. Undamped: [predict] applies the damping.
  double? get acceleration {
    if (_count < 3) return null;
    final dtOld = (_times[1] - _times[0]) / 1000.0;
    final dtNew = (_times[2] - _times[1]) / 1000.0;
    if (dtOld <= 0.0 || dtNew <= 0.0) return null;
    final vOld = (_values[1] - _values[0]) / dtOld;
    final vNew = (_values[2] - _values[1]) / dtNew;
    // Centred: the two velocities sit at the midpoints of their intervals.
    final span = (dtOld + dtNew) * 0.5;
    return (vNew - vOld) / span;
  }

  /// The newest sample's timestamp, for deciding whether the stream has starved.
  int? get newestTime => _count > 0 ? _times[_count - 1] : null;

  /// How many samples are held, 0 to 3.
  int get samples => _count;

  void reset() => _count = 0;
}
