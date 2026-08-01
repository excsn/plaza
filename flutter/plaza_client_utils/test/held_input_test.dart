import 'dart:math' as math;

import 'package:plaza_client_utils/plaza_client_utils.dart';
import 'package:test/test.dart';

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

/// One shared rule, exactly as both sides should have it.
P integrate(P p, double held, double dt, void _) => P(p.v + held * dt);
P lerp(P a, P b, double t) => P(a.v + (b.v - a.v) * t);
double distance(P a, P b) => (a.v - b.v).abs();

HeldInputPredictor<P, double, void> predictor(double blend) => HeldInputPredictor<P, double, void>(
      initial: const P(0.0),
      initialInput: 0.0,
      config: HeldInputConfig(blend: blend),
      integrate: integrate,
      lerp: lerp,
      context: null,
    );

/// A server that holds the direction and integrates it every tick, which is the
/// model this primitive exists for.
class HeldServer {
  double pos = 0.0;
  double held = 0.0;
  void step(double dt) => pos += held * dt;
}

/// Transliterated from `client_utils/src/held_input.rs`.
void main() {
  /// The whole promise: when the client runs the server's rule on the same held
  /// input, there is nothing to correct, however rarely input is transmitted.
  test('dead reckoning a held input matches the server exactly', () {
    final me = predictor(0.25)..hold(10.0);
    final server = HeldServer()..held = 10.0;

    for (var i = 0; i < 120; i++) {
      me.advance(1.0 / 60.0);
      server.step(1.0 / 60.0);
    }

    // Both are at the same simulated moment. The packet in flight describes the
    // server as it was one one-way delay ago, which is what the client receives.
    const age = 0.05;
    final sentAt = P(server.pos - 10.0 * age);
    final correction = me.reconcile(sentAt, age);
    expect(
      distance(correction.seen, correction.settled),
      lessThan(0.001),
      reason: 'a shared rule on a held input needs no correction',
    );
  });

  /// The bug this primitive is shaped to prevent, reproduced next to the fix: a
  /// slow systematic drift, corrected two different ways.
  test('a threshold snap sawtooths where a continuous blend does not', () {
    const driftPerPacket = 6.0;
    const threshold = 24.0;

    // Threshold policy: let it accumulate, then close the whole gap at once.
    var thresholdPos = 0.0;
    var server = 0.0;
    var biggestSnap = 0.0;
    for (var i = 0; i < 60; i++) {
      server += driftPerPacket;
      if ((server - thresholdPos).abs() > threshold) {
        biggestSnap = math.max(biggestSnap, (server - thresholdPos).abs());
        thresholdPos = server;
      }
    }

    // Continuous policy: the same drift, eased a fraction each packet.
    final me = predictor(0.25);
    server = 0.0;
    var biggestMove = 0.0;
    for (var i = 0; i < 60; i++) {
      server += driftPerPacket;
      final correction = me.reconcile(P(server), 0.0);
      biggestMove = math.max(biggestMove, distance(correction.seen, correction.settled));
    }

    expect(biggestSnap, greaterThan(threshold), reason: 'the threshold policy really does snap');
    expect(
      biggestMove,
      lessThan(biggestSnap / 2.0),
      reason: 'continuous easing must never move as far in one go',
    );
    expect(
      distance(me.logical, P(server)),
      lessThan(threshold),
      reason: 'and it still keeps up with the server',
    );
  });

  /// Correcting straight to an authoritative state pulls the entity backward by
  /// however far it travelled while the packet was in flight.
  test('a projection targets now rather than the packet\'s past', () {
    final me = predictor(1.0)..hold(100.0);
    me.advance(0.1);
    expect(me.logical.v, 10.0);

    // The server says 5.0, but that is 50ms old and the entity is still moving.
    final correction = me.reconcile(const P(5.0), 0.05);
    expect(correction.settled.v, closeTo(10.0, 1e-9), reason: '5.0 advanced by 50ms of held input');
  });

  test('a frozen entity tracks the server instead of predicting into it', () {
    final me = predictor(0.25)..hold(100.0);
    me.active = false;

    me.advance(1.0);
    expect(me.logical.v, 0.0, reason: 'a frozen entity does not dead reckon');

    final correction = me.reconcile(const P(42.0), 0.05);
    expect(correction.settled.v, 42.0, reason: 'it tracks the held position exactly');
    expect(me.logical.v, 42.0);

    me.active = true;
    me.advance(0.1);
    expect(me.logical.v, closeTo(52.0, 1e-3), reason: 'and resumes from there');
  });

  test('a discontinuity snaps while ordinary drift eases', () {
    final me = HeldInputPredictor<P, double, void>(
      initial: const P(0.0),
      initialInput: 0.0,
      config: const HeldInputConfig(blend: 0.25),
      integrate: integrate,
      lerp: lerp,
      context: null,
      distance: distance,
      teleportBeyond: 200.0,
    );

    // Ordinary drift: eased, so it does not arrive in one step.
    expect(me.reconcile(const P(40.0), 0.0).settled.v, lessThan(40.0), reason: 'drift is eased');

    // A respawn across the level: snapped, because the entity did not travel there
    // and easing would draw it through everything in between.
    expect(
      me.reconcile(const P(5000.0), 0.0).settled.v,
      5000.0,
      reason: 'a discontinuity arrives at once',
    );
  });

  test('a forced entity integrates its context', () {
    P withCurrent(P p, double held, double dt, double current) => P(p.v + (held + current) * dt);

    final me = HeldInputPredictor<P, double, double>(
      initial: const P(0.0),
      initialInput: 0.0,
      integrate: withCurrent,
      lerp: lerp,
      context: 3.0,
    )..hold(10.0);
    me.advance(1.0);
    expect(me.logical.v, 13.0, reason: "the world's push is part of the rule");
  });
}
