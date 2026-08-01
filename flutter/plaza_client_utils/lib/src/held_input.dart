import 'correction.dart';

/// How a [HeldInputPredictor] corrects itself.
class HeldInputConfig {
  const HeldInputConfig({this.blend = 0.25});

  /// The fraction of the remaining gap to the server closed on each reconcile.
  ///
  /// Higher converges faster and follows the server more tightly; lower stays
  /// smoother and leads more on local input. Zero disables correction entirely,
  /// which is pure dead reckoning and drifts without bound.
  final double blend;
}

/// A locally dead-reckoned entity whose server holds its input and integrates it
/// every tick.
///
/// The counterpart to [PredictedPlayer], which is for a server consuming one
/// input per step. Which one is right is decided by the server, not by taste:
/// sending repeats to a held-input server tells it nothing it does not know, and
/// dropping repeats from a per-step server drops actual movement.
///
/// There is no separate logical and render state here, unlike [PredictedPlayer]:
/// the correction is applied continuously to the state itself, so there is no
/// exact value being smoothed away.
///
/// Ported from `plaza_client_utils::held_input::HeldInputPredictor`.
class HeldInputPredictor<S, I, C> {
  HeldInputPredictor({
    required S initial,
    required I initialInput,
    required this.integrate,
    required this.lerp,
    required C context,
    HeldInputConfig config = const HeldInputConfig(),
    this.distance,
    this.teleportBeyond,
  })  : _state = initial,
        _held = initialInput,
        _blend = config.blend.clamp(0.0, 1.0),
        _ctx = context;

  /// The rule the *server* runs, shared rather than re-derived. Anything the
  /// server does that this leaves out arrives as a permanent correction, and
  /// tracking that down later costs far more than sharing the function now.
  final S Function(S state, I input, double dtSecs, C ctx) integrate;
  final S Function(S a, S b, double t) lerp;

  /// Opt-in discontinuity detection. Takes the metric rather than requiring one
  /// on the state type, so applications that do not want it are not made to
  /// define a distance.
  final double Function(S a, S b)? distance;

  /// Set well above any correction ordinary play produces: everything below is
  /// eased, and easing across a real teleport draws the entity smoothly through
  /// everything in between.
  final double? teleportBeyond;

  S _state;
  I _held;
  final double _blend;
  C _ctx;
  bool _active = true;

  /// Sets the input the server is holding. Call whenever the player's intent
  /// changes, independently of when it is transmitted: what is sent is a
  /// bandwidth decision, what is integrated is a simulation one.
  void hold(I input) => _held = input;

  I get held => _held;

  /// Dead reckons one step under the held input. Does nothing while frozen.
  void advance(double dtSecs) {
    if (_active) _state = integrate(_state, _held, dtSecs, _ctx);
  }

  /// Where the server's state has probably got to by now, given how old it is.
  ///
  /// An authoritative packet describes the past by one one-way delay, so
  /// correcting straight to it would pull the entity backward by whatever it
  /// travelled in the meantime. Advancing it by its own age under the held input
  /// is what makes the correction target *now*.
  ///
  /// Public so an application can measure the disagreement itself and decide what
  /// to do, instead of taking this type's policy.
  S project(S authoritative, double ageSecs) =>
      integrate(authoritative, _held, ageSecs, _ctx);

  /// Bends the prediction toward the server and reports the move.
  ///
  /// [ageSecs] is how old [authoritative] is, usually the one-way delay. While
  /// frozen this tracks the server exactly, since the server is not moving the
  /// entity and predicting into it would invent a correction every packet.
  Correction<S> reconcile(S authoritative, double ageSecs) {
    final seen = _state;
    if (!_active) {
      _state = authoritative;
      return Correction<S>(seen: seen, settled: _state);
    }

    final target = project(authoritative, ageSecs);
    final metric = distance;
    final beyond = teleportBeyond;
    final discontinuous =
        metric != null && beyond != null && metric(_state, target) > beyond;
    _state = discontinuous ? target : lerp(_state, target, _blend);
    return Correction<S>(seen: seen, settled: _state);
  }

  /// Moves the entity outright: a spawn, a respawn, a teleport. Not a correction,
  /// so nothing is eased.
  void teleport(S state) => _state = state;

  /// False while the server is holding this entity still: dead, stunned, mid
  /// respawn.
  set active(bool value) => _active = value;
  bool get active => _active;

  /// The world the integration runs against, for a forced entity.
  set context(C ctx) => _ctx = ctx;
  C get context => _ctx;

  /// Identical to [render]: there is no separate exact value to preserve.
  S get logical => _state;

  S render() => _state;
}
