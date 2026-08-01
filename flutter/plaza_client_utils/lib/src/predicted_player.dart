import 'correction.dart';
import 'prediction.dart';
import 'smoothing.dart';

/// How a [PredictedPlayer] is set up.
class PlayerConfig {
  const PlayerConfig({
    this.inputBuffer = 256,
    this.smoothingSecs = 0.1,
    this.easing = linear,
  });

  /// How many recent inputs to retain for replay. Cover the most that can be in
  /// flight at once: input rate times worst round trip.
  final int inputBuffer;

  /// How long a correction eases in the render. Zero snaps.
  final double smoothingSecs;

  final Easing easing;
}

/// The local player's entity: predicts on input, reconciles against the server,
/// and eases the correction.
///
/// Exposes both the exact [logical] state, for further game logic, and a smoothed
/// [render] state, for drawing. Keeping those separate is the point: game rules
/// must not read a state that has been smoothed for the eye.
///
/// This is the model for a server that consumes **one input per simulation step**.
/// For a server that holds an input and integrates it every tick, use
/// [HeldInputPredictor] instead; sending repeats to that one tells it nothing,
/// and dropping repeats from this one drops actual movement.
///
/// Ported from `plaza_client_utils::predicted_player::PredictedPlayer`.
class PredictedPlayer<S, I, C> {
  PredictedPlayer({
    required S initial,
    required this.apply,
    required this.lerp,
    required C context,
    PlayerConfig config = const PlayerConfig(),
  })  : _predicted = PredictedEntity<S, I>(initial),
        _inputs = ClientInputBuffer<I, S>(config.inputBuffer),
        _smoother = ErrorSmoother<S>(config.smoothingSecs, easing: config.easing),
        _ctx = context;

  /// The game rule. Takes the world as [C] so a *forced* entity, one the server
  /// moves by more than its own input, can run the same rule the server runs.
  final S Function(S state, I input, C ctx) apply;
  final S Function(S a, S b, double t) lerp;

  final PredictedEntity<S, I> _predicted;
  final ClientInputBuffer<I, S> _inputs;
  final ErrorSmoother<S> _smoother;
  int _nextSeq = 0;
  C _ctx;
  bool _active = true;

  /// Replaces the world the prediction runs against.
  ///
  /// Held rather than passed per input, so a replay uses the newest world rather
  /// than a snapshot per buffered input. That is a different approximation, not a
  /// strictly better one: the inputs being replayed happened under a world that
  /// has since moved. It is the cheap one, and over a few frames the difference
  /// is usually far smaller than the force being modelled. An application needing
  /// the exact history carries a snapshot in its own input type instead.
  set context(C ctx) => _ctx = ctx;
  C get context => _ctx;

  /// Freezes prediction, for an entity the server is holding still.
  set active(bool value) => _active = value;
  bool get active => _active;

  /// Moves the entity without easing, dropping pending inputs.
  ///
  /// A teleport is not a disagreement, and easing one draws the entity smoothly
  /// across the level through everything in between, which is worse than the snap
  /// it was avoiding. The pending inputs describe a journey that no longer
  /// happened.
  void teleport(S state) {
    _predicted.predicted = state;
    _predicted.authoritative = state;
    _inputs.clear();
    _smoother.reset();
  }

  /// Applies an input locally and records it for replay, returning the sequence
  /// number to send alongside it.
  ///
  /// While frozen the input is still numbered, so the sequence stays in step with
  /// the server, but nothing is predicted or recorded: there is no movement to
  /// replay over a state the server is holding still.
  int input(I input) {
    _nextSeq++;
    final seq = _nextSeq;
    if (!_active) return seq;
    _predicted.applyLocal(input, seq, _inputs, (s, i) => apply(s, i, _ctx));
    return seq;
  }

  /// Folds in the server's state: snaps the logical state to it, replays inputs
  /// the server had not processed, and begins easing the visible correction.
  ///
  /// Returns the correction as the state drawn beforehand and the state settled on
  /// afterwards, so a caller can measure its own prediction error without this
  /// type imposing a metric.
  Correction<S> reconcile(S authoritative, int ackedSeq) {
    final seen = render();
    if (_active) {
      _predicted.reconcile(authoritative, ackedSeq, _inputs, (s, i) => apply(s, i, _ctx));
    } else {
      // Frozen: nothing to replay over a state the server is holding. Track it
      // exactly and clear the buffer, or the first frame after unfreezing would
      // replay inputs from before the freeze.
      _predicted.predicted = authoritative;
      _predicted.authoritative = authoritative;
      _predicted.acknowledgedSeq = ackedSeq;
      _inputs.clear();
    }
    _smoother.beginFrom(seen);
    return Correction<S>(seen: seen, settled: _predicted.predicted);
  }

  /// Progresses the correction ease by one frame.
  void advance(double dtSecs) => _smoother.advance(dtSecs);

  /// Where to draw: the prediction, eased through recent corrections.
  S render() => _smoother.sample(_predicted.predicted, lerp);

  /// The exact predicted state, for game logic. Never smoothed.
  S get logical => _predicted.predicted;

  /// The last state the server confirmed, for a ghost overlay or an error readout.
  S get authoritative => _predicted.authoritative;

  int get latestSeq => _nextSeq;
  int get ackedSeq => _predicted.acknowledgedSeq;

  /// How many sent inputs still await acknowledgement, which is what a
  /// reconciliation replays.
  int get unackedCount => _inputs.unacknowledgedAfter(ackedSeq).length;

  bool get isEasing => _smoother.isEasing;
}
