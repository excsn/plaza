import 'dart:collection';

/// One input the client sent, with the state it was applied to.
class BufferedInput<Op, S> {
  const BufferedInput({
    required this.sequenceNumber,
    required this.op,
    required this.stateBeforeOp,
  });

  final int sequenceNumber;
  final Op op;

  /// The client's predicted state *before* this op was applied locally.
  final S stateBeforeOp;
}

/// A history of inputs sent to the server, for prediction and reconciliation.
///
/// Fixed capacity: the oldest is discarded when full, because an input older
/// than the buffer can no longer be replayed and holding it unbounded turns a
/// stalled server into a memory leak.
///
/// Ported from `plaza_client_utils::input_buffer::ClientInputBuffer`.
class ClientInputBuffer<Op, S> {
  /// Throws [ArgumentError] if [maxSize] is zero.
  ClientInputBuffer(this.maxSize) {
    if (maxSize <= 0) {
      throw ArgumentError.value(maxSize, 'maxSize', 'must be greater than zero');
    }
  }

  final int maxSize;
  final Queue<BufferedInput<Op, S>> _inputs = Queue<BufferedInput<Op, S>>();

  /// How many inputs were dropped because the buffer was full.
  ///
  /// Not in the Rust original, which logs a warning instead. A Dart library has
  /// no logger to reach for, and a counter is more useful anyway: a non-zero
  /// value means replay is already incomplete.
  int overflowed = 0;

  /// [stateBeforeOp] is the predicted state immediately before [op] was applied
  /// locally.
  void record(int sequenceNumber, Op op, S stateBeforeOp) {
    if (_inputs.length == maxSize) {
      _inputs.removeFirst();
      overflowed++;
    }
    _inputs.addLast(BufferedInput<Op, S>(
      sequenceNumber: sequenceNumber,
      op: op,
      stateBeforeOp: stateBeforeOp,
    ));
  }

  /// Drops everything up to and including [ackSequenceNumber]: the server has
  /// processed those, so they will never be replayed again.
  void acknowledgeUpTo(int ackSequenceNumber) {
    while (_inputs.isNotEmpty && _inputs.first.sequenceNumber <= ackSequenceNumber) {
      _inputs.removeFirst();
    }
  }

  /// The inputs after [ackSequenceNumber], in order. What reconciliation replays.
  Iterable<BufferedInput<Op, S>> unacknowledgedAfter(int ackSequenceNumber) =>
      _inputs.where((i) => i.sequenceNumber > ackSequenceNumber);

  /// The predicted state recorded before a given input, if it is still held.
  S? stateBefore(int sequenceNumber) {
    for (final i in _inputs) {
      if (i.sequenceNumber == sequenceNumber) return i.stateBeforeOp;
    }
    return null;
  }

  int get length => _inputs.length;
  bool get isEmpty => _inputs.isEmpty;
  void clear() => _inputs.clear();
}

/// An entity whose state the client predicts ahead of the server.
///
/// The loop is: apply an input locally and remember it, then when the server
/// acknowledges a sequence, snap to what it said and replay everything it had
/// not yet seen. What survives is the prediction the player is looking at.
///
/// Ported from `plaza_client_utils::prediction::PredictedEntity`.
class PredictedEntity<S, Op> {
  PredictedEntity(S initialState)
      : predicted = initialState,
        authoritative = initialState;

  /// What to draw.
  S predicted;

  /// The last state the server asserted.
  S authoritative;

  /// The newest input sequence the server has acknowledged.
  int acknowledgedSeq = 0;

  /// Applies an input locally and records it for replay.
  void applyLocal(
    Op op,
    int sequenceNumber,
    ClientInputBuffer<Op, S> buffer,
    S Function(S state, Op op) apply,
  ) {
    buffer.record(sequenceNumber, op, predicted);
    predicted = apply(predicted, op);
  }

  /// Snaps to the server's state and replays whatever it had not acknowledged.
  ///
  /// Returns how many inputs were replayed, which is the number a diagnostic
  /// wants: a replay count that grows without bound means acknowledgements are
  /// not arriving.
  int reconcile(
    S newAuthoritative,
    int serverAckSeq,
    ClientInputBuffer<Op, S> buffer,
    S Function(S state, Op op) apply,
  ) {
    authoritative = newAuthoritative;
    acknowledgedSeq = serverAckSeq;
    buffer.acknowledgeUpTo(serverAckSeq);
    predicted = authoritative;

    var replayed = 0;
    for (final input in buffer.unacknowledgedAfter(serverAckSeq)) {
      predicted = apply(predicted, input.op);
      replayed++;
    }
    return replayed;
  }
}
