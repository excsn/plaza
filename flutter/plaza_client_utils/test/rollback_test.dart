import 'package:plaza_client_utils/plaza_client_utils.dart';
import 'package:test/test.dart';

/// A tiny deterministic world: each player's position is an integer, moved by an
/// integer input. Integers keep re-simulation exactly comparable, no float drift.
class World {
  const World(this.a, this.b);
  final int a;
  final int b;

  @override
  bool operator ==(Object other) => other is World && other.a == a && other.b == b;

  @override
  int get hashCode => Object.hash(a, b);

  @override
  String toString() => 'World($a, $b)';
}

/// An input needs a value `==`, since that comparison is how a confirmation is
/// judged against the guess it replaces.
class In {
  const In(this.d);
  final int d;

  @override
  bool operator ==(Object other) => other is In && other.d == d;

  @override
  int get hashCode => d.hashCode;

  @override
  String toString() => 'In($d)';
}

const neutral = In(0);

World step(World state, List<In> inputs) =>
    World(state.a + inputs[0].d, state.b + inputs[1].d);

RollbackSession<World, In> session({int maxRollbackFrames = 64}) => RollbackSession<World, In>(
      initialState: const World(0, 0),
      neutralInputs: const [neutral, neutral],
      config: RollbackConfig(maxRollbackFrames: maxRollbackFrames),
      advance: step,
    );

/// A ground-truth simulation with every input known up front, for comparison.
World groundTruth(List<In> inputs0, List<In> inputs1) {
  var w = const World(0, 0);
  for (var f = 0; f < inputs0.length; f++) {
    w = step(w, [inputs0[f], inputs1[f]]);
  }
  return w;
}

/// Transliterated from `client_utils/src/rollback.rs`.
void main() {
  group('StateHistory', () {
    test('it saves, restores and evicts by frame', () {
      final h = StateHistory<int>(3)
        ..save(0, 10)
        ..save(1, 11)
        ..save(2, 12);
      expect(h.restore(1), 11);
      expect(h.oldestFrame, 0);
      expect(h.latestFrame, 2);

      h.save(3, 13);
      expect(h.restore(0), isNull, reason: 'frame 0 was evicted');
      expect(h.oldestFrame, 1);
      expect(h.restore(3), 13);
    });

    test('it overwrites a frame in place', () {
      final h = StateHistory<int>(4)
        ..save(0, 1)
        ..save(1, 2)
        ..save(1, 99);
      expect(h.restore(1), 99, reason: 're-simulation re-saves the same frame');
      expect(h.latestFrame, 1, reason: 'overwriting does not extend the window');
    });

    test('a non-contiguous save resets the window rather than leaving a gap', () {
      final h = StateHistory<int>(4)
        ..save(0, 1)
        ..save(1, 2)
        ..save(50, 3);
      expect(h.resets, 1);
      expect(h.oldestFrame, 50);
      expect(h.latestFrame, 50);
      expect(h.restore(1), isNull);
    });

    test('a zero capacity is rejected', () {
      expect(() => StateHistory<int>(0), throwsArgumentError);
    });

    test('the ring wraps without corrupting a frame', () {
      final h = StateHistory<int>(3);
      for (var f = 0; f < 20; f++) {
        h.save(f, f * 100);
      }
      expect(h.oldestFrame, 17);
      expect(h.latestFrame, 19);
      for (var f = 17; f <= 19; f++) {
        expect(h.restore(f), f * 100, reason: 'frame $f');
      }
      expect(h.restore(16), isNull);
      // Overwrite in place after wrapping, which is what a rollback does.
      h.save(18, -1);
      expect(h.restore(18), -1);
      expect(h.restore(17), 1700);
      expect(h.restore(19), 1900);
    });

    test('clear empties it', () {
      final h = StateHistory<int>(3)..save(7, 1);
      h.clear();
      expect(h.isEmpty, isTrue);
      expect(h.oldestFrame, isNull);
      expect(h.restore(7), isNull);
    });
  });

  group('InputTimeline', () {
    test('it predicts the last confirmed', () {
      final t = InputTimeline<In>(8)
        ..confirm(0, const In(5))
        ..confirm(1, const In(5));
      expect(t.confirmedAt(1), const In(5));
      expect(t.confirmedAt(2), isNull, reason: 'frame 2 is unconfirmed');
      expect(t.lastConfirmedAtOrBefore(5), const In(5), reason: 'the basis for predicting frame 5');
      expect(t.lastConfirmedFrame, 1);
    });

    test('a gap stays a gap until it is confirmed', () {
      final t = InputTimeline<In>(8)
        ..confirm(0, const In(1))
        ..confirm(3, const In(9));
      expect(t.confirmedAt(1), isNull);
      expect(t.confirmedAt(2), isNull);
      expect(t.lastConfirmedAtOrBefore(2), const In(1), reason: 'predicted from frame 0');

      // A resent input fills the gap out of order.
      t.confirm(1, const In(4));
      expect(t.confirmedAt(1), const In(4));
      expect(t.lastConfirmedAtOrBefore(2), const In(4));
      expect(t.lastConfirmedFrame, 3, reason: 'filling a gap does not move the newest back');
    });

    test('a frame older than the window is dropped', () {
      final t = InputTimeline<In>(3);
      for (var f = 0; f < 10; f++) {
        t.confirm(f, In(f));
      }
      expect(t.oldestFrame, 7);
      t.confirm(0, const In(99));
      expect(t.confirmedAt(0), isNull, reason: 'already past the rollback horizon');
      expect(t.oldestFrame, 7, reason: 'and it did not reset the window');
    });

    /// A jump wider than the window is handled in one move rather than by walking
    /// every skipped frame, and must land on the same window either way.
    test('a jump wider than the window leaves only the new frame', () {
      final t = InputTimeline<In>(4)
        ..confirm(0, const In(1))
        ..confirm(1, const In(2))
        ..confirm(900, const In(7));
      expect(t.oldestFrame, 897);
      expect(t.confirmedAt(900), const In(7));
      expect(t.confirmedAt(899), isNull);
      expect(t.confirmedAt(1), isNull);
      expect(t.lastConfirmedAtOrBefore(899), isNull, reason: 'nothing retained before it');
      expect(t.lastConfirmedFrame, 900);
    });

    test('a zero capacity is rejected', () {
      expect(() => InputTimeline<In>(0), throwsArgumentError);
    });
  });

  group('RollbackSession', () {
    /// The remote holds In(2) the whole time. Once repeat-last has a basis to
    /// repeat, it predicts the held input exactly, so lagging confirmations never
    /// contradict a guess.
    test('a correct prediction never rolls back', () {
      final s = session();
      const inputs = In(2);
      s.confirmRemoteInput(1, 0, inputs);
      for (var f = 0; f < 10; f++) {
        s.queueLocalInput(0, const In(1));
        if (f >= 3) s.confirmRemoteInput(1, f - 3, inputs);
        s.advanceFrame();
      }
      expect(s.rollbackCount, 0, reason: 'a held remote input is predicted exactly');
      expect(s.lastRollbackFrames, 0);
    });

    test('a wrong prediction rolls back and lands on the truth', () {
      // The remote changes direction at frame 4, which repeat-last cannot foresee.
      final remote = List<In>.generate(10, (f) => f < 4 ? const In(1) : const In(-3));
      final local = List<In>.filled(10, const In(1));

      final s = session();
      for (var f = 0; f < 10; f++) {
        s.queueLocalInput(0, local[f]);
        // The remote input for frame f arrives two frames late.
        if (f >= 2) s.confirmRemoteInput(1, f - 2, remote[f - 2]);
        s.advanceFrame();
      }
      // Drain the last in-flight confirmations and let it settle.
      for (var f = 8; f < 10; f++) {
        s.confirmRemoteInput(1, f, remote[f]);
      }
      s.resolvePendingRollback();

      expect(s.rollbackCount, greaterThan(0), reason: 'the direction change forced a rollback');
      expect(s.state, groundTruth(local, remote), reason: 'rollback converged on ground truth');
    });

    /// The determinism guarantee end to end: two independent sessions, each local to
    /// one player, each predicting the other, each rolling back. With every input
    /// eventually delivered they must agree, and agree with ground truth.
    test('two peers exchanging inputs converge to the same world', () {
      final p0 = List<In>.generate(40, (f) => In((f * 7) % 5 - 2));
      final p1 = List<In>.generate(40, (f) => In((f * 3) % 4 - 1));

      final a = session();
      final b = session();

      const delay = 2;
      for (var f = 0; f < 40; f++) {
        a.queueLocalInput(0, p0[f]);
        b.queueLocalInput(1, p1[f]);

        if (f >= delay) {
          final past = f - delay;
          a.confirmRemoteInput(1, past, p1[past]);
          b.confirmRemoteInput(0, past, p0[past]);
        }
        a.advanceFrame();
        b.advanceFrame();
      }
      // Flush the inputs still in flight when the loop ended.
      for (var f = 38; f < 40; f++) {
        a.confirmRemoteInput(1, f, p1[f]);
        b.confirmRemoteInput(0, f, p0[f]);
      }
      a.resolvePendingRollback();
      b.resolvePendingRollback();

      final truth = groundTruth(p0, p1);
      expect(a.state, truth, reason: 'peer A converged');
      expect(b.state, truth, reason: 'peer B converged');
      expect(a.state, b.state, reason: 'the two peers agree frame for frame');
    });

    test('a delay-based policy can tell when a frame is fully known', () {
      final s = session()..queueLocalInput(0, const In(1));
      expect(s.isFrameConfirmed(0), isFalse, reason: 'the remote input is not in yet');
      s.confirmRemoteInput(1, 0, const In(1));
      expect(s.isFrameConfirmed(0), isTrue);
    });

    /// Roll the window well past a frame, then confirm a contradicting input for
    /// that long-evicted frame. It cannot roll back that far, so it clamps to the
    /// oldest retained frame rather than corrupting state.
    test('a correction older than the history is clamped', () {
      final s = session(maxRollbackFrames: 4);
      for (var f = 0; f < 20; f++) {
        s.queueLocalInput(0, const In(1));
        s.confirmRemoteInput(1, f, const In(1));
        s.advanceFrame();
      }
      s.confirmRemoteInput(1, 0, const In(99));
      s.advanceFrame();
      expect(s.state.a, greaterThanOrEqualTo(0));
      expect(s.currentFrame, 21);
    });

    /// The same direction change, with rollback off: the misprediction is detected
    /// and then ignored, so the present never lands on the truth. The "why rollback"
    /// contrast.
    test('rollback disabled keeps a wrong guess and diverges from the truth', () {
      final remote = List<In>.generate(10, (f) => f < 4 ? const In(1) : const In(-3));
      final local = List<In>.filled(10, const In(1));

      final s = session()..rollbackEnabled = false;
      for (var f = 0; f < 10; f++) {
        s.queueLocalInput(0, local[f]);
        if (f >= 2) s.confirmRemoteInput(1, f - 2, remote[f - 2]);
        s.advanceFrame();
      }
      for (var f = 8; f < 10; f++) {
        s.confirmRemoteInput(1, f, remote[f]);
      }
      s.resolvePendingRollback();

      expect(s.rollbackCount, 0, reason: 'rollback disabled never re-simulates');
      expect(s.state, isNot(groundTruth(local, remote)),
          reason: 'without rollback the trusted guess never converges');
    });

    /// Two peers, predictions and rollbacks along the way. At a frame both have
    /// fully confirmed, their saved states are identical: that is the in-sync check.
    test('state at a confirmed frame matches across two peers', () {
      final p0 = List<In>.generate(30, (f) => In((f * 5) % 3 - 1));
      final p1 = List<In>.generate(30, (f) => In((f * 2) % 3 - 1));

      final a = session();
      final b = session();
      for (var f = 0; f < 30; f++) {
        a.queueLocalInput(0, p0[f]);
        b.queueLocalInput(1, p1[f]);
        if (f >= 3) {
          a.confirmRemoteInput(1, f - 3, p1[f - 3]);
          b.confirmRemoteInput(0, f - 3, p0[f - 3]);
        }
        a.advanceFrame();
        b.advanceFrame();
      }
      const cf = 20;
      expect(a.stateAt(cf), isNotNull, reason: 'A retains the frame');
      expect(a.stateAt(cf), b.stateAt(cf), reason: 'a fully-confirmed frame is identical');
    });

    test('the prediction horizon reflects how far ahead of confirmation it is', () {
      final s = session();
      for (var f = 0; f < 6; f++) {
        s.queueLocalInput(0, const In(1));
        s.advanceFrame();
      }
      // The local player is always confirmed; the remote has confirmed nothing, so
      // the horizon spans every simulated frame.
      expect(s.predictionHorizon, 6);
      s.confirmRemoteInput(1, 4, const In(0));
      expect(s.predictionHorizon, 1, reason: 'confirmed through frame 4, one frame ahead');
    });

    /// A predictor is a function of the last *confirmed* input, not of its own
    /// previous guess, so it cannot decay across a run of predicted frames: every
    /// one of them is guessed from the same basis. A predictor that wants to fade
    /// toward neutral has to do it from the frame number it is handed.
    test('a custom predictor replaces repeat-last', () {
      final halving = RollbackSession<World, In>(
        initialState: const World(0, 0),
        neutralInputs: const [neutral, neutral],
        advance: step,
        predictor: (last, frame) => In(last.d ~/ 2),
      );
      halving.confirmRemoteInput(1, 0, const In(8));
      for (var f = 0; f < 4; f++) {
        halving.queueLocalInput(0, const In(0));
        halving.advanceFrame();
      }
      // Frame 0 was confirmed at 8; frames 1 to 3 were each guessed at half of it.
      expect(halving.state.b, 8 + 4 + 4 + 4);

      // The truth was 8 throughout, so folding it in forces a rollback.
      for (var f = 1; f < 4; f++) {
        halving.confirmRemoteInput(1, f, const In(8));
      }
      halving.resolvePendingRollback();
      expect(halving.state.b, 32);
      expect(halving.rollbackCount, 1);
    });

    test('rollback depth is reported', () {
      final s = session();
      for (var f = 0; f < 6; f++) {
        s.queueLocalInput(0, const In(1));
        s.confirmRemoteInput(1, f, const In(1));
        s.advanceFrame();
      }
      expect(s.maxRollbackFrames, 0, reason: 'nothing has been mispredicted');

      // Contradict frame 1, five frames back.
      s.confirmRemoteInput(1, 1, const In(-5));
      s.advanceFrame();
      expect(s.lastRollbackFrames, 5, reason: 'frames 1 through 5 re-simulated');
      expect(s.maxRollbackFrames, 5);
      expect(s.rollbackCount, 1);
      expect(s.confirmedFrame(1), 5);
    });

    test('the current frame renders the present', () {
      final s = session()..queueLocalInput(0, const In(3));
      s.advanceFrame();
      expect(s.currentFrame, 1);
      expect(s.stateAt(1), s.state, reason: 'the head frame is the present');
      expect(s.stateAt(0), const World(0, 0), reason: 'and frame 0 is what it started from');
      expect(s.numPlayers, 2);
    });
  });
}
