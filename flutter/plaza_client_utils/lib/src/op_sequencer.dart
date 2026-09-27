import 'dart:async';
import 'dart:collection';

/// What an applier asks of the sequencer once it has applied an op.
///
/// A hold in seconds counts down on the frame's `dt`, lands on a frame boundary
/// and carries its remainder into the next hold, so a run of holds keeps time.
/// A hold on a future is released on the first pump after it completes, which
/// costs about half a frame per op and carries nothing. Use seconds when the
/// length is known and a future only when it is not.
sealed class Hold {
  const Hold();

  static const Hold none = HoldNone();

  const factory Hold.seconds(double seconds) = HoldSeconds;

  factory Hold.until(Future<void> future) = HoldUntil;
}

class HoldNone extends Hold {
  const HoldNone();
}

class HoldSeconds extends Hold {
  const HoldSeconds(this.seconds);

  final double seconds;
}

class HoldUntil extends Hold {
  HoldUntil(this.future);

  final Future<void> future;
}

/// Ops applied one at a time, with room for an animation between them.
///
/// A real-time client applies whatever arrived and draws the result. A
/// turn-based client has to show every op in order and one applied before the
/// previous animation finishes is one the player never saw. Ops queue here and
/// [pump], called from the game loop, releases them one at a time for as long
/// as the applier's holds allow.
///
/// Nothing is released outside a pump, so a stalled or backgrounded loop
/// holds the queue where it is.
class OpSequencer<T> {
  OpSequencer({this.maxQueued = 512});

  /// A backstop rather than a tuning knob: reaching it means ops arrive faster
  /// than they can be watched.
  final int maxQueued;

  final Queue<T> _queue = Queue<T>();
  double _hold = 0;
  bool _awaiting = false;
  int _generation = 0;
  int _dropped = 0;

  int get pending => _queue.length;

  bool get holding => _hold > 0 || _awaiting;

  /// Ops discarded because the queue was full.
  int get dropped => _dropped;

  void add(T op) {
    if (_queue.length >= maxQueued) {
      _dropped++;
      return;
    }
    _queue.add(op);
  }

  void addAll(Iterable<T> ops) => ops.forEach(add);

  /// Applies as many queued ops as the holds allow.
  ///
  /// One pump drains a run of ops that return [Hold.none] and stops at the
  /// first hold that outlasts the frame.
  void pump(double dt, Hold Function(T op) apply) {
    if (_awaiting) return;
    if (_hold > 0) {
      _hold -= dt;
      if (_hold > 0) return;
      dt = -_hold;
      _hold = 0;
    }
    while (_queue.isNotEmpty) {
      switch (apply(_queue.removeFirst())) {
        case HoldNone():
          continue;
        case HoldSeconds(:final seconds):
          _hold = seconds - dt;
          dt = 0;
          if (_hold <= 0) {
            _hold = 0;
            continue;
          }
          return;
        case HoldUntil(:final future):
          _awaiting = true;
          final generation = _generation;
          void release(_) {
            if (generation == _generation) _awaiting = false;
          }
          future.then(release, onError: release);
          return;
      }
    }
  }

  /// Drops the backlog, the hold in progress and any future being waited on,
  /// so the next pump starts fresh. For a resume or a resync, where the state
  /// arriving next makes everything queued stale.
  void clear() {
    _queue.clear();
    _hold = 0;
    _awaiting = false;
    _generation++;
  }
}
