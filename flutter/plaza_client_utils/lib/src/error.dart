/// Errors from these utilities.
///
/// Ported from `plaza_client_utils::error::ClientUtilError`. Dart has no
/// `thiserror`, so each variant is a class and the message lives in
/// `toString`.
sealed class ClientUtilError implements Exception {
  const ClientUtilError();
}

class InputBufferFull extends ClientUtilError {
  const InputBufferFull({required this.maxSize, required this.sequenceNumberTried});
  final int maxSize;
  final int sequenceNumberTried;

  @override
  String toString() => 'Input buffer is full. Maximum size: $maxSize. '
      'Cannot add new input with sequence: $sequenceNumberTried.';
}

class InputNotFoundInBuffer extends ClientUtilError {
  const InputNotFoundInBuffer(this.sequenceNumber);
  final int sequenceNumber;

  @override
  String toString() => 'Input with sequence number $sequenceNumber not found in buffer.';
}

class ReconciliationInconsistency extends ClientUtilError {
  const ReconciliationInconsistency({
    required this.serverAckSequence,
    this.clientLastKnownSequence,
  });
  final int serverAckSequence;
  final int? clientLastKnownSequence;

  @override
  String toString() => 'Cannot reconcile: Server acknowledged input sequence '
      '$serverAckSequence which was not found or is inconsistent with the client\'s '
      'input history (last known client sequence: $clientLastKnownSequence).';
}

class InvalidArgument extends ClientUtilError {
  const InvalidArgument(this.details);
  final String details;

  @override
  String toString() => 'Invalid argument provided: $details';
}
