import 'dart:collection';

/// Ops applied one at a time, with room for an animation between them.
///
/// A real-time game applies whatever arrived and draws the result, because only
/// the newest frame matters. In a turn-based game every op is an event the
/// player needs to see in order: a player who does not see the deal before the
/// first card lands has missed the game.
///
/// `PlazaClient.ops` delivers as fast as frames arrive, so this class sits
/// between the stream and the scene. Ops queue; [pump] releases them one at a
/// time and an op worth watching asks for a hold.
///
/// Nothing here knows what an op is. The caller's [Applier] does the work and
/// returns how long to wait before the next one, so the pacing lives with the
/// animation rather than in a table of durations.
class OpSequencer {
  OpSequencer({this.maxQueued = 512});

  /// A safety backstop only. Reaching it means ops are arriving faster than
  /// they can be watched, which for a turn-based game means something is wrong
  /// upstream rather than that the queue needs to be bigger.
  final int maxQueued;

  final Queue<Object?> _queue = Queue<Object?>();
  double _hold = 0;
  int _dropped = 0;

  /// Ops waiting to be shown.
  int get pending => _queue.length;

  /// Whether an animation is currently holding the queue.
  bool get holding => _hold > 0;

  /// Ops discarded because the queue was full. A number rather than a silent
  /// truncation, because a sequencer that quietly drops is indistinguishable
  /// from a server that never sent.
  int get dropped => _dropped;

  void add(Object? op) {
    if (_queue.length >= maxQueued) {
      _dropped++;
      return;
    }
    _queue.add(op);
  }

  void addAll(Iterable<Object?> ops) => ops.forEach(add);

  /// Applies as many queued ops as the holds allow.
  ///
  /// `apply` returns the seconds to wait before the next op, so returning zero
  /// means "nothing to watch here, keep going". A single [pump] therefore
  /// drains a run of instant ops and stops at the first one worth seeing.
  void pump(double dt, double Function(Object? op) apply) {
    if (_hold > 0) {
      _hold -= dt;
      if (_hold > 0) return;
      // Whatever is left over is spent on the next op rather than discarded, or
      // a slow frame silently stretches every hold in the queue.
      dt = -_hold;
      _hold = 0;
    }
    while (_queue.isNotEmpty && _hold <= 0) {
      final wait = apply(_queue.removeFirst());
      if (wait > 0) {
        _hold = wait - dt;
        dt = 0;
        if (_hold <= 0) {
          _hold = 0;
          continue;
        }
        return;
      }
    }
  }

  /// Drops everything queued, for a resync.
  ///
  /// A resumed client is sent fresh state and replaying a backlog on top of it
  /// would animate stale state. The transport drops its own backlog for the
  /// same reason.
  void clear() => _queue.clear();
}
