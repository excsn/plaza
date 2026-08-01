import 'extrapolation.dart';
import 'interpolation.dart';

/// What [RemoteView.render] should do.
class RenderOpts {
  const RenderOpts({this.interpolate = true, this.extrapolate = false});

  /// Interpolate at the target time. False draws the raw newest snapshot, which
  /// jumps at the server rate.
  final bool interpolate;

  /// Dead-reckon along the last velocity when the buffer has no snapshot ahead of
  /// the target, instead of holding the newest.
  final bool extrapolate;
}

/// One remote entity's samples, and the decision about what to draw from them.
///
/// Holds the interpolate, extrapolate or hold choice internally and returns the
/// right state, rather than handing a caller a starvation callback to invert
/// control over. Which of the three happened is visible from the options and the
/// buffer, not from a branch the application has to write.
///
/// Extrapolation caps the *duration*, so the entity coasts to the limit and
/// stops there. Returning the raw newest sample past the limit is the obvious
/// reading of "clamp" and it is a discontinuity: at the limit the entity has
/// coasted `velocity * cap` forward, and one millisecond later it would be drawn
/// back at the sample, a jump of the whole extrapolation window in the wrong
/// direction, flickering under jitter around the boundary.
///
/// Ported from `plaza_client_utils::remote_view::RemoteView`.
class RemoteView<S, V> {
  RemoteView({
    required int bufferSize,
    required this.maxExtrapolationMs,
    required this.lerp,
    required this.extrapolateBy,
  }) : _buffer = SnapshotBuffer<S>(maxSize: bufferSize, lerp: lerp);

  /// How far past the newest sample dead reckoning may reach.
  final int maxExtrapolationMs;

  final S Function(S a, S b, double t) lerp;

  /// Advances a state along a velocity by a number of seconds.
  final S Function(S state, V velocity, double dtSecs) extrapolateBy;

  final SnapshotBuffer<S> _buffer;
  (int, S, V)? _latest;

  /// How many renders asked for a time further past the newest sample than
  /// [maxExtrapolationMs], and were served the capped coast instead.
  ///
  /// Not in the Rust original, which logs a warning. Holding at the cap is a
  /// legitimate outcome, so this is not an error, but reaching it *steadily* means
  /// this entity's packets have stopped arriving and the view is drawing a guess
  /// that has stopped improving.
  int overExtrapolations = 0;

  /// Records a sample. The velocity is kept beside it for extrapolation.
  void push(int timeMs, S state, V velocity) {
    _buffer.add(timeMs, state);
    final latest = _latest;
    if (latest == null || timeMs >= latest.$1) {
      _latest = (timeMs, state, velocity);
    }
  }

  /// What to draw at [target], or null before the first sample.
  S? render(int? target, [RenderOpts opts = const RenderOpts()]) {
    final latest = _latest;
    if (latest == null) return null;
    if (!opts.interpolate) return latest.$2;
    if (target == null) return latest.$2;

    final newest = _buffer.newestTimestamp;
    if (opts.extrapolate && newest != null && target > newest) {
      if (target - newest > maxExtrapolationMs) overExtrapolations++;
      // The cap rule lives in one place, so this view and a hand-rolled one
      // cannot drift apart on the boundary behaviour.
      final base = ExtrapolationBase<S, V>(
        state: latest.$2,
        velocity: latest.$3,
        serverTimestamp: newest,
        clientReceiptTimeMs: newest,
        extrapolateBy: extrapolateBy,
      );
      return base.at(target, maxExtrapolationMs);
    }
    return _buffer.at(target) ?? latest.$2;
  }

  S? get latest => _latest?.$2;
  V? get latestVelocity => _latest?.$3;
  int? get latestTimestamp => _latest?.$1;
  int? get oldestTimestamp => _buffer.oldestTimestamp;
  int get length => _buffer.length;
  bool get isEmpty => _buffer.isEmpty;

  void clear() {
    _buffer.clear();
    _latest = null;
  }
}
