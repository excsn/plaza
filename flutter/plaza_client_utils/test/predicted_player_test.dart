import 'package:plaza_client_utils/plaza_client_utils.dart';
import 'package:test/test.dart';

/// A one-dimensional position, so the arithmetic is readable.
class P {
  const P(this.v);
  final double v;

  @override
  bool operator ==(Object other) => other is P && other.v == v;

  @override
  int get hashCode => v.hashCode;

  @override
  String toString() => 'P($v)';
}

P apply(P p, double i, void _) => P(p.v + i);
P lerp(P a, P b, double t) => P(a.v + (b.v - a.v) * t);

PredictedPlayer<P, double, void> player(double smoothingSecs) => PredictedPlayer<P, double, void>(
      initial: const P(0.0),
      config: PlayerConfig(inputBuffer: 64, smoothingSecs: smoothingSecs),
      apply: apply,
      lerp: lerp,
      context: null,
    );

/// Transliterated from `client_utils/src/predicted_player.rs`.
void main() {
  test('predicting moves the logical state', () {
    final me = player(0.0)
      ..input(1.0)
      ..input(1.0);
    expect(me.logical.v, 2.0);
  });

  test('reconciliation replays unacknowledged inputs', () {
    final me = player(0.0);
    final s1 = me.input(1.0);
    me.input(1.0);

    // The server has only processed the first input: state 1 as of s1.
    me.reconcile(const P(1.0), s1);

    expect(me.logical.v, 2.0, reason: 'snap to 1, then replay the unacked second input');
    expect(me.unackedCount, 1);
  });

  test('a correction eases the render but not the logical', () {
    final me = player(0.1);
    final s = me.input(10.0);

    // The server disagrees: the true state is 0.
    me.reconcile(const P(0.0), s);
    expect(me.logical.v, 0.0, reason: 'logical snaps to authority immediately');
    expect(me.render().v, closeTo(10.0, 1e-3), reason: 'render starts where the eye was');

    me.advance(0.05);
    expect(me.render().v, closeTo(5.0, 0.2), reason: 'render eases halfway');

    me.advance(0.05);
    expect(me.render().v, closeTo(0.0, 1e-3), reason: 'render arrives at the logical state');
  });

  /// The lesson a real game paid for: an entity the server moves by more than its
  /// own input has to run the same rule, and that rule needs the world. With
  /// nowhere to put the world, a client writes a second, lesser rule and drifts by
  /// the whole size of the force it left out.
  test('a forced entity predicts the force from its context', () {
    P applyWithWind(P p, double i, double wind) => P(p.v + i + wind);

    final me = PredictedPlayer<P, double, double>(
      initial: const P(0.0),
      config: const PlayerConfig(inputBuffer: 64, smoothingSecs: 0.0),
      apply: applyWithWind,
      lerp: lerp,
      context: 0.5,
    )
      ..input(1.0)
      ..input(1.0);
    expect(me.logical.v, 3.0, reason: 'each step carries the input plus the wind');

    // The server agrees, because it ran the same rule. Nothing to correct.
    final correction = me.reconcile(const P(3.0), me.latestSeq);
    expect(correction.seen, correction.settled, reason: 'a matching rule needs no correction');
  });

  /// A server holding an entity still (dead, stunned, mid respawn) keeps reporting
  /// the same position. A client that keeps integrating input into it manufactures
  /// a correction every single packet, out of nothing.
  test('a frozen entity stops predicting instead of inventing corrections', () {
    final me = player(0.0)..input(1.0);
    expect(me.logical.v, 1.0);

    me.active = false;
    me
      ..input(1.0)
      ..input(1.0);
    expect(me.logical.v, 1.0, reason: 'a frozen entity does not move on input');

    final correction = me.reconcile(const P(1.0), me.latestSeq);
    expect(correction.seen, correction.settled, reason: 'and so it never disagrees with the server');
    expect(me.unackedCount, 0, reason: 'nothing is queued for replay while frozen');

    me.active = true;
    me.input(1.0);
    expect(me.logical.v, 2.0, reason: 'it picks back up without replaying the frozen inputs');
  });

  /// A correction is a disagreement about a path and must be eased. A teleport is
  /// not a disagreement at all: easing it draws the entity smoothly across
  /// everything in between.
  test('a teleport snaps and drops the journey', () {
    final me = player(0.5)..input(1.0);
    me.reconcile(const P(50.0), 0);
    expect(me.render().v, lessThan(50.0), reason: 'an ordinary correction eases');

    me.teleport(const P(900.0));
    expect(me.render().v, 900.0, reason: 'a teleport is visible immediately');
    expect(me.logical.v, 900.0);
    expect(me.unackedCount, 0, reason: 'pending inputs described a journey that did not happen');
  });

  test('reconcile reports what it corrected', () {
    final me = player(0.0)
      ..input(1.0)
      ..input(1.0);
    // The server only got the first input, and disagrees about where it led.
    final correction = me.reconcile(const P(10.0), 1);
    expect(correction.seen.v, 2.0, reason: 'where it was being drawn');
    expect(correction.settled.v, 11.0, reason: '10 authoritative, replaying the unacked second input');
  });

  test('overflowing the input buffer stays exact inside the retained window', () {
    final me = PredictedPlayer<P, double, void>(
      initial: const P(0.0),
      config: const PlayerConfig(inputBuffer: 4, smoothingSecs: 0.0),
      apply: apply,
      lerp: lerp,
      context: null,
    );
    for (var i = 0; i < 20; i++) {
      me.input(1.0);
    }
    expect(me.logical.v, 20.0, reason: 'prediction advanced through all inputs');

    // Reconcile to an ack still inside the retained window: replay is exact.
    me.reconcile(const P(17.0), me.latestSeq - 3);
    expect(me.logical.v, 20.0, reason: '17 authoritative, replay 18/19/20 back to 20');
  });

  test('reconciling with a future ack snaps and clears', () {
    final me = player(0.0)
      ..input(1.0)
      ..input(1.0);
    // The server claims to have processed more than was sent. It should not happen,
    // and must not misbehave: everything acknowledges, nothing replays.
    me.reconcile(const P(42.0), 9999);
    expect(me.logical.v, 42.0);
    expect(me.unackedCount, 0);
  });

  test('zero smoothing renders the logical at once', () {
    final me = player(0.0);
    final s = me.input(10.0);
    me.reconcile(const P(0.0), s);
    expect(me.render().v, 0.0, reason: 'no ease, render is the logical state');
  });
}
