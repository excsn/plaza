/// The default catch-up cap: a quarter of a second, or fifteen steps at 60Hz.
///
/// Enough that an ordinary hitch is caught up smoothly, small enough that a
/// resumed tab skips ahead instead of grinding through the minutes it was asleep.
const int defaultMaxFrameMs = 250;

/// The nanosecond step `1.0 / hz` names, the same value the Rust side computes
/// with `Duration::from_secs_f64`, pinned across the languages by the
/// `fixed_timestep_hz` and `periodic_hz` golden vectors.
int _nanosOfHz(int hz) => (1.0 / hz * 1e9).round();

/// The steps one [FixedTimestep.advance] paid for.
///
/// Each item is the step duration in nanoseconds, which is the value the
/// simulation must advance by ([FixedTimestep.stepSecs] is the seconds form).
/// Taking it from here rather than from the frame delta is what stops a caller
/// stepping by the wrong amount.
class Steps extends Iterable<int> {
  Steps(this._count, this.stepNanos);

  final int _count;
  final int stepNanos;

  @override
  int get length => _count;

  @override
  Iterator<int> get iterator => _StepsIterator(_count, stepNanos);
}

class _StepsIterator implements Iterator<int> {
  _StepsIterator(this._remaining, this._stepNanos);
  int _remaining;
  final int _stepNanos;

  @override
  late int current;

  @override
  bool moveNext() {
    if (_remaining == 0) return false;
    _remaining--;
    current = _stepNanos;
    return true;
  }
}

/// Turns real elapsed time into a whole number of fixed simulation steps.
///
/// Engine-agnostic on purpose. Some Dart engines provide a fixed step and some do
/// not, and a building block cannot assume one does.
///
/// Ported from `plaza_client_utils::timestep::FixedTimestep`. Internals are
/// integer nanoseconds like the Rust side's, so a rate that does not divide a
/// round number of milliseconds carries exactly, and nothing accumulates float
/// error.
class FixedTimestep {
  /// Throws [ArgumentError] if [stepMs] is zero, which would make every frame an
  /// infinite loop.
  FixedTimestep.fromStepMs(int stepMs, {this.maxFrameMs = defaultMaxFrameMs})
      : _stepNanos = stepMs * 1000000 {
    if (stepMs <= 0) {
      throw ArgumentError.value(stepMs, 'stepMs', 'must be greater than zero');
    }
  }

  /// A step of exactly `1.0 / hz` seconds, to the nanosecond.
  ///
  /// The same value `plaza::TickDriver::from_hz` and the Rust timestep compute,
  /// so a Dart client stepping from here means the same thing by a rate as the
  /// driver the server runs on: 60Hz is 16.666667ms on every side. A
  /// millisecond step would make 16 of it and run 4.2% fast against the
  /// server, which reads as a permanent correction.
  factory FixedTimestep.fromHz(int hz, {int maxFrameMs = defaultMaxFrameMs}) {
    if (hz <= 0) {
      throw ArgumentError.value(hz, 'hz', 'must be greater than zero');
    }
    return FixedTimestep._(_nanosOfHz(hz), maxFrameMs);
  }

  FixedTimestep._(this._stepNanos, this.maxFrameMs);

  int _stepNanos;
  int _accumulatedNanos = 0;
  int _droppedNanos = 0;

  /// The most elapsed time one [advance] will pay for.
  ///
  /// Lower means a resumed tab catches up less and skips more; higher means it
  /// catches up more and risks a visible hitch doing it.
  int maxFrameMs;

  /// Adds elapsed time and returns the steps it pays for.
  ///
  /// The accumulator is drained here rather than as the steps are consumed, so
  /// the time is spent whether or not the caller runs every step.
  Steps advance(int elapsedMs) {
    final elapsedNanos = elapsedMs * 1000000;
    final maxFrameNanos = maxFrameMs * 1000000;
    if (elapsedNanos > maxFrameNanos) _droppedNanos += elapsedNanos - maxFrameNanos;
    _accumulatedNanos += elapsedNanos < maxFrameNanos ? elapsedNanos : maxFrameNanos;
    final count = _accumulatedNanos ~/ _stepNanos;
    _accumulatedNanos -= count * _stepNanos;
    return Steps(count, _stepNanos);
  }

  int get stepNanos => _stepNanos;

  /// Changes the step, keeping whatever has accumulated, so a change takes effect
  /// from now rather than stalling.
  ///
  /// Changing the step of a *simulation* is not free the way changing a send rate
  /// is: the step size is part of the rule, so two peers integrating at different
  /// steps diverge even running identical code.
  set stepNanos(int value) {
    if (value <= 0) {
      throw ArgumentError.value(value, 'stepNanos', 'must be greater than zero');
    }
    _stepNanos = value;
  }

  /// See [stepNanos].
  set stepMs(int value) {
    if (value <= 0) {
      throw ArgumentError.value(value, 'stepMs', 'must be greater than zero');
    }
    _stepNanos = value * 1000000;
  }

  double get stepSecs => _stepNanos / 1e9;

  /// Time carried over, always less than one step, in whole milliseconds.
  int get pendingMs => _accumulatedNanos ~/ 1000000;

  /// How far between the last step and the next, 0 to 1.
  ///
  /// For rendering between fixed steps: interpolating the drawn state by this
  /// removes the stutter a fixed step shows when the step rate and the refresh
  /// rate disagree. Worth knowing it exists, because the usual first diagnosis of
  /// that stutter is that the step rate is too low.
  double get alpha => _accumulatedNanos / _stepNanos;

  /// Elapsed time the catch-up cap refused, in total whole milliseconds. Real
  /// time the simulation never ran.
  ///
  /// Non-zero after a tab was backgrounded or a machine slept, and worth
  /// surfacing: a world quietly behind wall time explains a whole class of "it
  /// desynced and I do not know when".
  int get droppedMs => _droppedNanos ~/ 1000000;

  /// Discards the carried remainder, for a world that has been rebuilt. Leaves
  /// [droppedMs] alone, which is a session total.
  void reset() => _accumulatedNanos = 0;
}

/// Something that should happen every interval, driven by elapsed time.
///
/// The same accumulator as [FixedTimestep] with a different consumption rule, and
/// separate because the two answer different questions. A fixed step asks "how
/// much simulation does this frame pay for", where every step must run or the
/// world falls behind. A period asks "is it time yet", where the work is usually
/// idempotent and running it twice in one frame is waste rather than correctness.
///
/// Ported from `plaza_client_utils::timestep::Periodic`.
class Periodic {
  Periodic(int intervalMs) : _intervalNanos = intervalMs * 1000000 {
    if (intervalMs <= 0) {
      throw ArgumentError.value(intervalMs, 'intervalMs', 'must be greater than zero');
    }
  }

  /// A period of exactly `1.0 / hz` seconds, the same expression as
  /// [FixedTimestep.fromHz].
  factory Periodic.fromHz(int hz) {
    if (hz <= 0) {
      throw ArgumentError.value(hz, 'hz', 'must be greater than zero');
    }
    return Periodic._(_nanosOfHz(hz));
  }

  Periodic._(this._intervalNanos);

  int _intervalNanos;
  int _accumulatedNanos = 0;

  int get intervalNanos => _intervalNanos;

  /// Keeps whatever has accumulated, so a change takes effect from now rather
  /// than restarting the period.
  set intervalNanos(int value) {
    if (value <= 0) {
      throw ArgumentError.value(value, 'intervalNanos', 'must be greater than zero');
    }
    _intervalNanos = value;
  }

  /// See [intervalNanos].
  set intervalMs(int value) {
    if (value <= 0) {
      throw ArgumentError.value(value, 'intervalMs', 'must be greater than zero');
    }
    _intervalNanos = value * 1000000;
  }

  /// Adds elapsed time and says whether the period elapsed, at most once.
  ///
  /// The remainder carries, so the average rate stays exact. Time beyond a single
  /// interval is *kept*, not discarded, so a long frame is repaid on the following
  /// ones rather than resetting the phase.
  bool due(int elapsedMs) {
    _accumulatedNanos += elapsedMs * 1000000;
    if (_accumulatedNanos >= _intervalNanos) {
      _accumulatedNanos -= _intervalNanos;
      return true;
    }
    return false;
  }

  /// How many whole periods the elapsed time covers.
  ///
  /// For work where each occurrence matters (spawning a wave, firing a weapon),
  /// as opposed to work that is idempotent within a frame.
  int advance(int elapsedMs) {
    _accumulatedNanos += elapsedMs * 1000000;
    final count = _accumulatedNanos ~/ _intervalNanos;
    _accumulatedNanos -= count * _intervalNanos;
    return count;
  }

  /// How long until the period next elapses, in whole milliseconds.
  int get remainingMs {
    final left = _intervalNanos - _accumulatedNanos;
    return left < 0 ? 0 : left ~/ 1000000;
  }

  void reset() => _accumulatedNanos = 0;
}
