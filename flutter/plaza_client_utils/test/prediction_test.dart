import 'package:plaza_client_utils/plaza_client_utils.dart';
import 'package:test/test.dart';

class PlayerState {
  const PlayerState(this.x, this.y);
  final int x;
  final int y;

  @override
  bool operator ==(Object other) => other is PlayerState && other.x == x && other.y == y;

  @override
  int get hashCode => Object.hash(x, y);

  @override
  String toString() => 'PlayerState($x, $y)';
}

class Move {
  const Move(this.dx, this.dy);
  final int dx;
  final int dy;

  @override
  bool operator ==(Object other) => other is Move && other.dx == dx && other.dy == dy;

  @override
  int get hashCode => Object.hash(dx, dy);
}

PlayerState applyMove(PlayerState state, Move op) => PlayerState(state.x + op.dx, state.y + op.dy);

/// Transliterated from `client_utils/src/input_buffer.rs` and
/// `client_utils/src/prediction.rs`.
void main() {
  group('ClientInputBuffer', () {
    test('a new buffer is empty', () {
      final buffer = ClientInputBuffer<Move, PlayerState>(3);
      expect(buffer.isEmpty, isTrue);
      expect(buffer.length, 0);
    });

    test('a zero size is rejected', () {
      expect(() => ClientInputBuffer<Move, PlayerState>(0), throwsArgumentError);
    });

    test('recording keeps the op and the state before it', () {
      final buffer = ClientInputBuffer<Move, PlayerState>(3)
        ..record(1, const Move(10, 0), const PlayerState(0, 0))
        ..record(2, const Move(20, 0), const PlayerState(10, 0));

      expect(buffer.length, 2);
      final unacked = buffer.unacknowledgedAfter(0).toList();
      expect(unacked.length, 2);
      expect(unacked[0].sequenceNumber, 1);
      expect(unacked[0].op, const Move(10, 0));
      expect(unacked[0].stateBeforeOp, const PlayerState(0, 0));
      expect(unacked[1].sequenceNumber, 2);
    });

    test('the oldest input is dropped when full', () {
      final buffer = ClientInputBuffer<Move, PlayerState>(2)
        ..record(1, const Move(10, 0), const PlayerState(0, 0))
        ..record(2, const Move(20, 0), const PlayerState(10, 0));
      expect(buffer.length, 2);

      buffer.record(3, const Move(30, 0), const PlayerState(20, 0));
      expect(buffer.length, 2);

      final unacked = buffer.unacknowledgedAfter(0).toList();
      expect(unacked.map((i) => i.sequenceNumber), [2, 3], reason: 'input 1 is gone');
    });

    /// Not in the Rust original, which logs a warning instead. A dropped input
    /// means replay is already incomplete, which is worth a number rather than a
    /// log line nobody reads.
    test('a dropped input is counted', () {
      final buffer = ClientInputBuffer<Move, PlayerState>(2);
      for (var i = 1; i <= 5; i++) {
        buffer.record(i, const Move(1, 0), const PlayerState(0, 0));
      }
      expect(buffer.overflowed, 3);
    });

    test('acknowledging drops everything up to and including the sequence', () {
      final buffer = ClientInputBuffer<Move, PlayerState>(3)
        ..record(1, const Move(10, 0), const PlayerState(0, 0))
        ..record(2, const Move(20, 0), const PlayerState(10, 0))
        ..record(3, const Move(30, 0), const PlayerState(20, 0));

      buffer.acknowledgeUpTo(1);
      expect(buffer.length, 2);
      expect(buffer.unacknowledgedAfter(1).first.sequenceNumber, 2);

      buffer.acknowledgeUpTo(3);
      expect(buffer.isEmpty, isTrue);
      expect(buffer.unacknowledgedAfter(3), isEmpty);
    });

    test('acknowledging a sequence that was never sent still drops the older ones', () {
      final buffer = ClientInputBuffer<Move, PlayerState>(3)
        ..record(10, const Move(100, 0), const PlayerState(0, 0))
        ..record(12, const Move(120, 0), const PlayerState(100, 0));

      buffer.acknowledgeUpTo(11);
      expect(buffer.length, 1);
      expect(buffer.unacknowledgedAfter(0).first.sequenceNumber, 12);

      buffer.acknowledgeUpTo(9);
      expect(buffer.length, 1, reason: 'older than anything held, so nothing changes');
    });

    test('unacknowledgedAfter selects strictly greater sequences', () {
      final buffer = ClientInputBuffer<Move, PlayerState>(5);
      for (var i = 1; i <= 4; i++) {
        buffer.record(i, Move(i * 10, 0), const PlayerState(0, 0));
      }

      expect(buffer.unacknowledgedAfter(0).length, 4);
      expect(buffer.unacknowledgedAfter(1).map((i) => i.sequenceNumber), [2, 3, 4]);
      expect(buffer.unacknowledgedAfter(3).map((i) => i.sequenceNumber), [4]);
      expect(buffer.unacknowledgedAfter(4), isEmpty);
      expect(buffer.unacknowledgedAfter(5), isEmpty, reason: 'an ack beyond what was sent');
    });

    test('the state before an input is recoverable while it is held', () {
      final buffer = ClientInputBuffer<Move, PlayerState>(3)
        ..record(1, const Move(10, 0), const PlayerState(0, 0))
        ..record(2, const Move(20, 0), const PlayerState(10, 0))
        ..record(3, const Move(30, 0), const PlayerState(20, 0));

      expect(buffer.stateBefore(1), const PlayerState(0, 0));
      expect(buffer.stateBefore(2), const PlayerState(10, 0));
      expect(buffer.stateBefore(3), const PlayerState(20, 0));
      expect(buffer.stateBefore(4), isNull);
    });

    test('clear empties it', () {
      final buffer = ClientInputBuffer<Move, PlayerState>(3)
        ..record(1, const Move(10, 0), const PlayerState(0, 0))
        ..record(2, const Move(20, 0), const PlayerState(10, 0));
      expect(buffer.isEmpty, isFalse);
      buffer.clear();
      expect(buffer.isEmpty, isTrue);
      expect(buffer.length, 0);
    });
  });

  group('PredictedEntity', () {
    test('it starts agreeing with itself', () {
      const initial = PlayerState(0, 0);
      final entity = PredictedEntity<PlayerState, Move>(initial);
      expect(entity.predicted, initial);
      expect(entity.authoritative, initial);
      expect(entity.acknowledgedSeq, 0);
    });

    test('a local input moves the prediction and is recorded', () {
      final entity = PredictedEntity<PlayerState, Move>(const PlayerState(0, 0));
      final buffer = ClientInputBuffer<Move, PlayerState>(10);

      entity.applyLocal(const Move(1, 0), 1, buffer, applyMove);

      expect(entity.predicted, const PlayerState(1, 0));
      expect(buffer.length, 1);
      final buffered = buffer.unacknowledgedAfter(0).first;
      expect(buffered.sequenceNumber, 1);
      expect(buffered.op, const Move(1, 0));
      expect(buffered.stateBeforeOp, const PlayerState(0, 0));
    });

    test('a confirmed prediction with nothing outstanding settles on the server state', () {
      final entity = PredictedEntity<PlayerState, Move>(const PlayerState(0, 0));
      final buffer = ClientInputBuffer<Move, PlayerState>(10);
      entity.applyLocal(const Move(5, 0), 1, buffer, applyMove);

      const authoritative = PlayerState(5, 0);
      final replayed = entity.reconcile(authoritative, 1, buffer, applyMove);

      expect(entity.predicted, authoritative);
      expect(entity.authoritative, authoritative);
      expect(entity.acknowledgedSeq, 1);
      expect(buffer.isEmpty, isTrue);
      expect(replayed, 0);
    });

    test('a mispredicted input with nothing outstanding snaps to the server state', () {
      final entity = PredictedEntity<PlayerState, Move>(const PlayerState(0, 0));
      final buffer = ClientInputBuffer<Move, PlayerState>(10);
      entity.applyLocal(const Move(5, 0), 1, buffer, applyMove);

      const authoritative = PlayerState(3, 0);
      entity.reconcile(authoritative, 1, buffer, applyMove);

      expect(entity.predicted, authoritative);
      expect(entity.authoritative, authoritative);
      expect(buffer.isEmpty, isTrue);
    });

    test('unacknowledged inputs are replayed on top of the server state', () {
      final entity = PredictedEntity<PlayerState, Move>(const PlayerState(0, 0));
      final buffer = ClientInputBuffer<Move, PlayerState>(10);
      entity.applyLocal(const Move(5, 0), 1, buffer, applyMove);
      entity.applyLocal(const Move(2, 0), 2, buffer, applyMove);
      entity.applyLocal(const Move(1, 0), 3, buffer, applyMove);

      final replayed = entity.reconcile(const PlayerState(5, 0), 1, buffer, applyMove);

      expect(entity.authoritative, const PlayerState(5, 0));
      expect(entity.acknowledgedSeq, 1);
      expect(entity.predicted, const PlayerState(8, 0), reason: '5 plus the two unacked moves');
      expect(buffer.length, 2);
      expect(buffer.unacknowledgedAfter(1).first.sequenceNumber, 2);
      expect(replayed, 2, reason: 'the replay count is what a diagnostic watches');
    });

    test('a misprediction and outstanding inputs resolve together', () {
      final entity = PredictedEntity<PlayerState, Move>(const PlayerState(0, 0));
      final buffer = ClientInputBuffer<Move, PlayerState>(10);
      entity.applyLocal(const Move(10, 0), 1, buffer, applyMove);
      entity.applyLocal(const Move(2, 0), 2, buffer, applyMove);

      entity.reconcile(const PlayerState(5, 0), 1, buffer, applyMove);

      expect(entity.authoritative, const PlayerState(5, 0));
      expect(entity.acknowledgedSeq, 1);
      expect(entity.predicted, const PlayerState(7, 0), reason: '5 authoritative, replaying +2');
      expect(buffer.length, 1);
      expect(buffer.unacknowledgedAfter(1).first.sequenceNumber, 2);
    });
  });
}
