/// A logical simulation frame. Rollback counts in fixed frames, not wall time:
/// two peers agree on "frame 900", never on a millisecond.
typedef Frame = int;

/// A frame-indexed ring of whole-world state snapshots.
///
/// Rollback saves the state at the start of every frame and, on a misprediction,
/// restores the one at the frame that went wrong. Frames are contiguous: you save
/// `f`, then `f + 1`, and so on; re-simulation saves the same frames again, which
/// overwrites in place. Only the most recent [capacity] frames are kept, which is
/// the maximum distance you can ever roll back.
///
/// Ported from `plaza_client_utils::rollback::StateHistory`.
class StateHistory<S> {
  /// Keeps at most [capacity] frames of history.
  ///
  /// Throws [ArgumentError] if [capacity] is zero or negative.
  StateHistory(this.capacity) : _slots = List<Object?>.filled(capacity > 0 ? capacity : 1, null) {
    if (capacity <= 0) {
      throw ArgumentError.value(capacity, 'capacity', 'must be greater than zero');
    }
  }

  final int capacity;

  /// A fixed ring. Re-simulation overwrites frames already inside the window on
  /// every rolled-back frame, so that path must not allocate.
  final List<Object?> _slots;
  int _start = 0;
  int _len = 0;
  Frame _baseFrame = 0;

  /// How many saves fell outside the window and reset it.
  ///
  /// Not in the Rust original, which logs a warning. A counter is more useful in a
  /// library with no logger to reach for: non-zero means frames were saved
  /// non-contiguously, which rollback assumes never happens.
  int resets = 0;

  /// Records the state at [frame].
  ///
  /// The intended use is contiguous: append `frame == latest + 1`, or overwrite a
  /// frame already inside the window (re-simulation does this). A save that skips
  /// ahead of the window resets it, so the buffer never holds a gap.
  void save(Frame frame, S state) {
    if (_len == 0) {
      _baseFrame = frame;
      _slots[_start] = state;
      _len = 1;
      return;
    }

    final end = _baseFrame + _len;
    if (frame == end) {
      if (_len == capacity) {
        _slots[_start] = state;
        _start = (_start + 1) % capacity;
        _baseFrame++;
      } else {
        _slots[(_start + _len) % capacity] = state;
        _len++;
      }
    } else if (frame >= _baseFrame && frame < end) {
      _slots[(_start + (frame - _baseFrame)) % capacity] = state;
    } else {
      resets++;
      _start = 0;
      _len = 1;
      _baseFrame = frame;
      _slots[0] = state;
    }
  }

  /// The state saved at [frame], if still retained. Null if it was evicted or
  /// never saved.
  S? restore(Frame frame) {
    if (_len == 0 || frame < _baseFrame) return null;
    final offset = frame - _baseFrame;
    if (offset >= _len) return null;
    return _slots[(_start + offset) % capacity] as S?;
  }

  /// The oldest frame still retained, or null if empty.
  Frame? get oldestFrame => _len == 0 ? null : _baseFrame;

  /// The newest frame saved, or null if empty.
  Frame? get latestFrame => _len == 0 ? null : _baseFrame + _len - 1;

  int get length => _len;
  bool get isEmpty => _len == 0;

  void clear() {
    _slots.fillRange(0, capacity, null);
    _start = 0;
    _len = 0;
    _baseFrame = 0;
  }
}

/// The inputs known for one input source (one player), by frame, with the gaps
/// predicted.
///
/// A confirmed input is one the source actually produced; an unconfirmed frame is
/// **predicted** by repeating the last confirmed input at or before it. That is
/// the standard rollback guess, and it is right whenever a player holds a
/// direction, which is most of the time. [RollbackSession] compares a later
/// confirmation against what it predicted to decide whether to roll back.
///
/// Ported from `plaza_client_utils::rollback::InputTimeline`.
class InputTimeline<I> {
  /// Retains inputs across at most [capacity] frames.
  ///
  /// Throws [ArgumentError] if [capacity] is zero or negative.
  InputTimeline(this.capacity) : _slots = List<Object?>.filled(capacity > 0 ? capacity : 1, null) {
    if (capacity <= 0) {
      throw ArgumentError.value(capacity, 'capacity', 'must be greater than zero');
    }
  }

  final int capacity;

  /// Null in a slot inside the window means that frame is a gap: known to exist,
  /// not yet confirmed.
  final List<Object?> _slots;
  int _start = 0;
  int _len = 0;
  Frame _baseFrame = 0;
  Frame? _lastConfirmed;

  /// Records the real input the source produced for [frame].
  ///
  /// Frames may arrive out of order, since a resent input can fill a gap left by a
  /// lost packet; any missing frames between the window and [frame] are held as
  /// gaps until they too are confirmed. A frame older than the retained window is
  /// dropped, it is already past the rollback horizon.
  void confirm(Frame frame, I input) {
    if (_len == 0) {
      _baseFrame = frame;
      _slots[_start] = input;
      _len = 1;
    } else if (frame < _baseFrame) {
      return;
    } else {
      final end = _baseFrame + _len;
      if (frame < end) {
        _slots[(_start + (frame - _baseFrame)) % capacity] = input;
      } else {
        final gap = frame - end;
        if (gap + 1 >= capacity) {
          // The jump is wider than the window, so nothing retained survives it.
          // Landing straight on the final window avoids walking the whole gap.
          _slots.fillRange(0, capacity, null);
          _start = 0;
          _len = capacity;
          _baseFrame = frame - capacity + 1;
          _slots[capacity - 1] = input;
        } else {
          for (var i = 0; i < gap; i++) {
            _push(null);
          }
          _push(input);
        }
      }
    }
    final last = _lastConfirmed;
    _lastConfirmed = last == null || frame > last ? frame : last;
  }

  void _push(Object? value) {
    if (_len == capacity) {
      _slots[_start] = value;
      _start = (_start + 1) % capacity;
      _baseFrame++;
    } else {
      _slots[(_start + _len) % capacity] = value;
      _len++;
    }
  }

  /// The confirmed input at [frame], or null if that frame is unconfirmed
  /// (predicted) or outside the window.
  I? confirmedAt(Frame frame) {
    if (_len == 0 || frame < _baseFrame) return null;
    final offset = frame - _baseFrame;
    if (offset >= _len) return null;
    return _slots[(_start + offset) % capacity] as I?;
  }

  /// The most recent confirmed input at or before [frame]: the basis for
  /// predicting [frame] when it is not itself confirmed.
  I? lastConfirmedAtOrBefore(Frame frame) {
    if (_len == 0 || frame < _baseFrame) return null;
    final newest = _baseFrame + _len - 1;
    var f = frame < newest ? frame : newest;
    while (true) {
      final input = _slots[(_start + (f - _baseFrame)) % capacity];
      if (input != null) return input as I;
      if (f == _baseFrame) return null;
      f--;
    }
  }

  /// The newest frame ever confirmed for this source, or null if none has been.
  Frame? get lastConfirmedFrame => _lastConfirmed;

  /// The oldest frame still inside the window, or null if empty.
  Frame? get oldestFrame => _len == 0 ? null : _baseFrame;

  int get length => _len;
  bool get isEmpty => _len == 0;
}

/// How a [RollbackSession] is set up.
class RollbackConfig {
  const RollbackConfig({this.maxRollbackFrames = 240});

  /// The furthest back the session can roll, in frames. It bounds the state and
  /// input history retained, so it must comfortably exceed the worst prediction
  /// horizon (round-trip latency in frames). Default 240, four seconds at 60fps.
  final int maxRollbackFrames;
}

/// The default input predictor: repeat the last confirmed input unchanged.
///
/// This is right whenever a player holds their input steady, which dominates most
/// games, and it is what a session uses unless another rule is supplied.
I repeatLastInput<I>(I last, Frame frame) => last;

/// The whole rollback loop for one peer, wired.
///
/// It owns a [StateHistory], an [InputTimeline] per player, and the current frame,
/// and drives the predict / detect / rollback / re-simulate cycle against a
/// deterministic step you supply. The primitives stay public for anyone who wants
/// to wire the loop differently; this is the ready-made path, the rollback
/// counterpart to `PredictedPlayer` for the authoritative model.
///
/// Each peer runs its own session and calls its local player index the "local"
/// one; the two are otherwise identical, which is the point, both re-simulate to
/// the same state from the same inputs.
///
/// [I] must have a meaningful `==`: that comparison is how a confirmation is
/// judged against the guess it replaces. A type with identity equality reports
/// every confirmation as a misprediction.
///
/// ```dart
/// final session = RollbackSession<World, Move>(
///   initialState: world,
///   neutralInputs: [Move.none, Move.none],
///   advance: step,
/// );
///
/// session.queueLocalInput(local, myInput);
/// for (final (frame, input) in inbox) {
///   session.confirmRemoteInput(remote, frame, input);
/// }
/// session.advanceFrame();
/// draw(session.state);
/// ```
///
/// Ported from `plaza_client_utils::rollback::RollbackSession`.
class RollbackSession<S, I> {
  /// Creates a session over `neutralInputs.length` players, starting from
  /// [initialState]. `neutralInputs[p]` is the input assumed for player `p` before
  /// any of its inputs are known, typically "no input".
  RollbackSession({
    required S initialState,
    required List<I> neutralInputs,
    required this.advance,
    RollbackConfig config = const RollbackConfig(),
    this.predictor,
  })  : _neutral = List<I>.of(neutralInputs),
        _currentState = initialState,
        _stateHistory = StateHistory<S>(_capacityOf(config)),
        _used = List<StateHistory<I>>.generate(
          neutralInputs.length,
          (_) => StateHistory<I>(_capacityOf(config)),
        ),
        _timelines = List<InputTimeline<I>>.generate(
          neutralInputs.length,
          (_) => InputTimeline<I>(_capacityOf(config)),
        );

  static int _capacityOf(RollbackConfig config) =>
      config.maxRollbackFrames < 1 ? 1 : config.maxRollbackFrames;

  /// The deterministic step: same state and inputs in, same state out, every time
  /// and on every peer, which is what rollback rests on.
  final S Function(S state, List<I> inputs) advance;

  /// Takes the last confirmed input and the frame being predicted, and returns the
  /// guess for that frame.
  ///
  /// Null means [repeatLastInput], which Dart cannot name as a default value here
  /// because a constant tearoff may not close over a type parameter.
  final I Function(I last, Frame frame)? predictor;

  final StateHistory<S> _stateHistory;

  /// What input was actually fed to each player for each simulated frame, so a
  /// later confirmation can be checked against the guess that was used.
  final List<StateHistory<I>> _used;
  final List<InputTimeline<I>> _timelines;

  /// The neutral input assumed for a player before it has confirmed anything.
  final List<I> _neutral;

  S _currentState;
  Frame _headFrame = 0;
  bool _rollbackEnabled = true;
  Frame? _earliestIncorrect;
  int _lastRollbackLen = 0;
  int _maxRollbackLen = 0;
  int _rollbackCount = 0;

  int get numPlayers => _timelines.length;

  /// The next frame to be simulated. [state] is the world at the start of this
  /// frame, that is, after every frame before it.
  Frame get currentFrame => _headFrame;

  /// The world as it stands now: the present the peer renders. Includes the effect
  /// of every predicted input still awaiting confirmation.
  S get state => _currentState;

  /// The world at the start of [frame], if still retained.
  ///
  /// This is the *saved* state, so for a fully confirmed frame it is identical on
  /// every peer; that equality is the determinism guarantee, and comparing two
  /// peers here is how a demo shows they are in sync. Returns the present for the
  /// current frame.
  S? stateAt(Frame frame) {
    if (frame == _headFrame) return _currentState;
    return _stateHistory.restore(frame);
  }

  /// Turns rollback on or off (on by default). With it off the session still
  /// predicts and advances, but never restores or re-simulates: it trusts every
  /// guess permanently. That is not a way to ship, since predictions that are
  /// never corrected drift a peer out of sync, but it isolates what rollback buys,
  /// and it is the mechanism a delay-based front end disables when it waits for
  /// inputs instead of predicting them.
  set rollbackEnabled(bool enabled) => _rollbackEnabled = enabled;
  bool get rollbackEnabled => _rollbackEnabled;

  /// Supplies the local player's input for the current frame. Local inputs are
  /// known before their frame runs, so they are never mispredicted. Call once per
  /// frame before [advanceFrame].
  void queueLocalInput(int player, I input) =>
      _timelines[player].confirm(_headFrame, input);

  /// Folds in a remote input that has arrived for a past or current [frame]. If it
  /// contradicts the guess already used for an *already-simulated* frame, the
  /// session marks that frame for rollback on the next [advanceFrame].
  void confirmRemoteInput(int player, Frame frame, I input) {
    _timelines[player].confirm(frame, input);

    if (frame < _headFrame) {
      final used = _used[player].restore(frame);
      if (used != null && used != input) {
        final current = _earliestIncorrect;
        _earliestIncorrect = current == null || frame < current ? frame : current;
      }
    }
  }

  /// Advances the simulation by one frame: first rolls back and re-simulates if a
  /// confirmation disproved a guess, then simulates the current frame, predicting
  /// any input not yet known.
  void advanceFrame() {
    resolvePendingRollback();
    _simulateFrame(_headFrame);
    _headFrame++;
  }

  /// Applies any pending correction without simulating a new frame.
  ///
  /// [advanceFrame] does this first, so a normal loop never calls it. It is public
  /// here because the last confirmations of a session arrive after its final
  /// frame, and settling on them is the only way to compare the present against a
  /// fully-known ground truth. The Rust original keeps this private and reaches it
  /// from its own tests, which a separate Dart test file cannot do.
  void resolvePendingRollback() {
    _lastRollbackLen = 0;
    var earliest = _earliestIncorrect;
    _earliestIncorrect = null;
    if (earliest == null) return;
    // The guess is kept and never corrected: the "why rollback" comparison.
    if (!_rollbackEnabled) return;
    // Nothing simulated yet went wrong.
    if (earliest >= _headFrame) return;

    // Clamp to what is still retained: a correction older than the history can no
    // longer be applied exactly. Re-simulating from the oldest kept frame is the
    // closest the session can get, and keeps it from diverging further.
    final oldest = _stateHistory.oldestFrame;
    if (oldest == null) return;
    if (earliest < oldest) earliest = oldest;

    final restored = _stateHistory.restore(earliest);
    if (restored == null) return;

    final target = _headFrame;
    _currentState = restored;
    for (var f = earliest; f < target; f++) {
      _simulateFrame(f);
    }

    final length = target - earliest;
    _lastRollbackLen = length;
    if (length > _maxRollbackLen) _maxRollbackLen = length;
    _rollbackCount++;
  }

  /// Whether every player's input for [frame] is confirmed (none predicted).
  ///
  /// A delay-based peer waits for this before advancing; a rollback peer ignores
  /// it and predicts. Which to do is the application's policy, so this only
  /// reports.
  bool isFrameConfirmed(Frame frame) =>
      _timelines.every((t) => t.confirmedAt(frame) != null);

  /// The newest frame this player's input is confirmed through, or null.
  Frame? confirmedFrame(int player) => _timelines[player].lastConfirmedFrame;

  /// How many frames the present is running ahead of the least-confirmed player:
  /// the depth of prediction currently exposed to a rollback. Zero when every
  /// input is known up to the last simulated frame.
  int get predictionHorizon {
    if (_headFrame == 0) return 0;
    final lastSimulated = _headFrame - 1;
    var worst = 0;
    for (final timeline in _timelines) {
      final confirmed = timeline.lastConfirmedFrame;
      final depth = confirmed == null
          ? _headFrame
          : (confirmed >= lastSimulated ? 0 : lastSimulated - confirmed);
      if (depth > worst) worst = depth;
    }
    return worst;
  }

  /// Frames re-simulated by the most recent [advanceFrame], zero if it did not
  /// roll back.
  int get lastRollbackFrames => _lastRollbackLen;

  /// The deepest rollback seen so far, in frames.
  int get maxRollbackFrames => _maxRollbackLen;

  /// How many times the session has rolled back.
  int get rollbackCount => _rollbackCount;

  /// Simulates one frame from the present: saves the pre-state, gathers each
  /// player's input (confirmed or predicted), records what it used, and steps.
  void _simulateFrame(Frame frame) {
    _stateHistory.save(frame, _currentState);

    final inputs = List<I>.generate(_timelines.length, (p) {
      final input = _inputFor(p, frame);
      _used[p].save(frame, input);
      return input;
    }, growable: false);

    _currentState = advance(_currentState, inputs);
  }

  /// Player [p]'s input for [frame]: the confirmed value if known, otherwise the
  /// predictor applied to the last confirmed input, or to the neutral input if the
  /// player has confirmed nothing yet.
  I _inputFor(int p, Frame frame) {
    final confirmed = _timelines[p].confirmedAt(frame);
    if (confirmed != null) return confirmed;
    final basis = _timelines[p].lastConfirmedAtOrBefore(frame) ?? _neutral[p];
    final predict = predictor;
    return predict == null ? basis : predict(basis, frame);
  }
}
