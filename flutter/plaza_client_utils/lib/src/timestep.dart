/// The default catch-up cap: a quarter of a second, or fifteen steps at 60Hz.
///
/// Enough that an ordinary hitch is caught up smoothly, small enough that a
/// resumed tab skips ahead instead of grinding through the minutes it was asleep.
const int defaultMaxFrameMs = 250;

/// The steps one [FixedTimestep.advance] paid for.
///
/// Each item is the step duration in milliseconds, which is the value the
/// simulation must advance by. Taking it from here rather than from the frame
/// delta is what stops a caller stepping by the wrong amount.
class Steps extends Iterable<int> {
  Steps(this._count, this.stepMs);

  final int _count;
  final int stepMs;

  @override
  int get length => _count;

  @override
  Iterator<int> get iterator => _StepsIterator(_count, stepMs);
}

class _StepsIterator implements Iterator<int> {
  _StepsIterator(this._remaining, this._stepMs);
  int _remaining;
  final int _stepMs;

  @override
  late int current;

  @override
  bool moveNext() {
    if (_remaining == 0) return false;
    _remaining--;
    current = _stepMs;
    return true;
  }
}

/// Turns real elapsed time into a whole number of fixed simulation steps.
///
/// Engine-agnostic on purpose. Some Dart engines provide a fixed step and some do
/// not, and a building block cannot assume one does.
///
/// Ported from `plaza_client_utils::timestep::FixedTimestep`.
class FixedTimestep {
  /// Throws [ArgumentError] if [stepMs] is zero, which would make every frame an
  /// infinite loop.
  FixedTimestep.fromStepMs(int stepMs, {this.maxFrameMs = defaultMaxFrameMs})
      : _stepMs = stepMs {
    if (stepMs <= 0) {
      throw ArgumentError.value(stepMs, 'stepMs', 'must be greater than zero');
    }
  }

  /// A step of `1000 ~/ hz`.
  ///
  /// Integer division, so rates that do not divide 1000 evenly truncate: 60Hz is
  /// 16ms rather than 16.667. Deliberate at millisecond resolution, and it means
  /// both sides of a wire agree exactly as long as they agree on the rate.
  factory FixedTimestep.fromHz(int hz, {int maxFrameMs = defaultMaxFrameMs}) {
    if (hz <= 0 || hz > 1000) {
      throw ArgumentError.value(hz, 'hz', 'must be between 1 and 1000');
    }
    return FixedTimestep.fromStepMs(1000 ~/ hz, maxFrameMs: maxFrameMs);
  }

  int _stepMs;
  int _accumulatedMs = 0;
  int _droppedMs = 0;

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
    if (elapsedMs > maxFrameMs) _droppedMs += elapsedMs - maxFrameMs;
    _accumulatedMs += elapsedMs < maxFrameMs ? elapsedMs : maxFrameMs;
    final count = _accumulatedMs ~/ _stepMs;
    _accumulatedMs -= count * _stepMs;
    return Steps(count, _stepMs);
  }

  int get stepMs => _stepMs;

  /// Changes the step, keeping whatever has accumulated, so a change takes effect
  /// from now rather than stalling.
  ///
  /// Changing the step of a *simulation* is not free the way changing a send rate
  /// is: the step size is part of the rule, so two peers integrating at different
  /// steps diverge even running identical code.
  set stepMs(int value) {
    if (value <= 0) {
      throw ArgumentError.value(value, 'stepMs', 'must be greater than zero');
    }
    _stepMs = value;
  }

  double get stepSecs => _stepMs / 1000.0;

  /// Time carried over, always less than one step.
  int get pendingMs => _accumulatedMs;

  /// How far between the last step and the next, 0 to 1.
  ///
  /// For rendering between fixed steps: interpolating the drawn state by this
  /// removes the stutter a fixed step shows when the step rate and the refresh
  /// rate disagree. Worth knowing it exists, because the usual first diagnosis of
  /// that stutter is that the step rate is too low.
  double get alpha => _accumulatedMs / _stepMs;

  /// Elapsed time the catch-up cap refused, in total. Real time the simulation
  /// never ran.
  ///
  /// Non-zero after a tab was backgrounded or a machine slept, and worth
  /// surfacing: a world quietly behind wall time explains a whole class of "it
  /// desynced and I do not know when".
  int get droppedMs => _droppedMs;

  /// Discards the carried remainder, for a world that has been rebuilt. Leaves
  /// [droppedMs] alone, which is a session total.
  void reset() => _accumulatedMs = 0;
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
  Periodic(int intervalMs) : _intervalMs = intervalMs {
    if (intervalMs <= 0) {
      throw ArgumentError.value(intervalMs, 'intervalMs', 'must be greater than zero');
    }
  }

  factory Periodic.fromHz(int hz) {
    if (hz <= 0 || hz > 1000) {
      throw ArgumentError.value(hz, 'hz', 'must be between 1 and 1000');
    }
    return Periodic(1000 ~/ hz);
  }

  int _intervalMs;
  int _accumulatedMs = 0;

  int get intervalMs => _intervalMs;

  /// Keeps whatever has accumulated, so a change takes effect from now rather
  /// than restarting the period.
  set intervalMs(int value) {
    if (value <= 0) {
      throw ArgumentError.value(value, 'intervalMs', 'must be greater than zero');
    }
    _intervalMs = value;
  }

  /// Adds elapsed time and says whether the period elapsed, at most once.
  ///
  /// The remainder carries, so the average rate stays exact. Time beyond a single
  /// interval is *kept*, not discarded, so a long frame is repaid on the following
  /// ones rather than resetting the phase.
  bool due(int elapsedMs) {
    _accumulatedMs += elapsedMs;
    if (_accumulatedMs >= _intervalMs) {
      _accumulatedMs -= _intervalMs;
      return true;
    }
    return false;
  }

  /// How many whole periods the elapsed time covers.
  ///
  /// For work where each occurrence matters (spawning a wave, firing a weapon),
  /// as opposed to work that is idempotent within a frame.
  int advance(int elapsedMs) {
    _accumulatedMs += elapsedMs;
    final count = _accumulatedMs ~/ _intervalMs;
    _accumulatedMs -= count * _intervalMs;
    return count;
  }

  int get remainingMs {
    final left = _intervalMs - _accumulatedMs;
    return left < 0 ? 0 : left;
  }

  void reset() => _accumulatedMs = 0;
}
