/// Maps a progress in 0..1 to an eased progress in 0..1.
typedef Easing = double Function(double t);

/// The identity: a constant-speed catch-up. The default.
double linear(double t) => t;

/// Eased in and out with zero velocity at both ends. The usual choice for
/// hiding a correction.
double smoothstep(double t) => t * t * (3.0 - 2.0 * t);

/// Quick to start, settling softly. Good when the correction should visibly
/// begin at once but land gently.
double easeOutCubic(double t) {
  final u = 1.0 - t;
  return 1.0 - u * u * u;
}

/// Barely moves at first, then rushes.
///
/// Wrong for a reconciliation correction, which wants to start immediately and
/// land softly. Right for something drawn toward a target under a force that
/// grows as it closes.
double easeInCubic(double t) => t * t * t;

/// The gentler ease-in.
///
/// Prefer this to [easeInCubic] whenever the motion has to stay *visible* for
/// its whole duration. Cubic covers only 12.5% of the distance in the first half
/// of the time, which over a short animation reads as an object sitting still
/// and then teleporting rather than accelerating. Quadratic covers 25% and still
/// finishes fast.
double easeInQuad(double t) => t * t;

/// Gentle at both ends, a lighter smoothstep.
double easeInOutQuad(double t) {
  if (t < 0.5) return 2.0 * t * t;
  final u = -2.0 * t + 2.0;
  return 1.0 - u * u / 2.0;
}

/// Eases a rendered position toward a logical one after a correction.
///
/// Holds no copy of the logical state: the live value is passed to [sample]
/// each frame, which returns where to actually draw. The blend *across states*
/// is the `lerp` you supply; the easing only says how far along to be. The two
/// are independent.
///
/// Ported from `plaza_client_utils::smoothing::ErrorSmoother`.
class ErrorSmoother<S> {
  /// A duration of zero makes every correction snap, which disables smoothing
  /// without branching at the call site.
  ErrorSmoother(double durationSecs, {this.easing = linear})
      : _duration = durationSecs < 0 ? 0 : durationSecs;

  final double _duration;
  Easing easing;

  S? _from;
  double _elapsed = 0;

  /// Starts easing from the position the entity was last drawn at. Call right
  /// after a reconciliation whose jump should be hidden. Calling again mid-ease
  /// restarts from the new point.
  void beginFrom(S renderedBeforeCorrection) {
    if (_duration <= 0) {
      _from = null;
      return;
    }
    _from = renderedBeforeCorrection;
    _elapsed = 0;
  }

  /// Advances by one frame. No effect when not easing.
  void advance(double dtSecs) {
    if (_from == null) return;
    _elapsed += dtSecs;
    if (_elapsed >= _duration) _from = null;
  }

  /// Where to draw this frame.
  ///
  /// While easing, blends from the captured pre-correction position toward the
  /// live [logical] state, which keeps moving as prediction continues.
  S sample(S logical, S Function(S a, S b, double t) lerp) {
    final from = _from;
    if (from == null || _duration <= 0) return logical;
    final progress = (_elapsed / _duration).clamp(0.0, 1.0);
    return lerp(from, logical, easing(progress));
  }

  /// Abandons any ease in progress.
  ///
  /// For a discontinuity, where the entity did not travel from where it was
  /// drawn to where it now is: a teleport, a respawn, a level load. Easing
  /// across one of those slides the entity through everything in between, which
  /// is a worse artefact than the snap the ease exists to avoid.
  void reset() {
    _from = null;
    _elapsed = 0;
  }

  bool get isEasing => _from != null;
}
