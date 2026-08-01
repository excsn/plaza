import 'types.dart';

/// The last authoritative state and velocity for an entity, as the basis for
/// extrapolation.
///
/// Extrapolation predicts an entity's state a short way past the newest
/// authoritative update, to mask latency. The Rust original takes the step as an
/// `Extrapolatable` trait implementation; here it is [extrapolateBy], a function,
/// which is how the rest of this port passes rules.
///
/// Ported from `plaza_client_utils::extrapolation::ExtrapolationBase`.
class ExtrapolationBase<S, V> {
  ExtrapolationBase({
    required this.state,
    required this.velocity,
    required this.serverTimestamp,
    required this.clientReceiptTimeMs,
    required this.extrapolateBy,
  });

  /// The last authoritative state received from the server.
  final S state;

  /// The last authoritative velocity received from the server.
  final V velocity;

  /// The server's timestamp when this state and velocity were authoritative.
  final int serverTimestamp;

  /// The client's local time when this update was processed, which is what says
  /// how old the data is.
  final ClientTimeMs clientReceiptTimeMs;

  /// Advances a state along a velocity by a number of seconds.
  final S Function(S state, V velocity, double dtSecs) extrapolateBy;

  /// How many calls to [at] asked for a time past [clientReceiptTimeMs] by more
  /// than the cap they were given.
  ///
  /// The Rust original logs a warning here, and says at length what it usually
  /// means: reaching this *steadily* is almost never a starved link, it is a
  /// **render target computed the wrong way**. A target derived from an absolute
  /// clock estimate sits ahead of the newest sample by the whole link delay, so
  /// the view never interpolates and every entity is drawn held or dead reckoned.
  /// The symptom on screen is remote entities that stutter or overshoot.
  int overExtrapolations = 0;

  /// The state extrapolated to [targetClientRenderTimeMs], capped at
  /// [maxExtrapolationDurationMs] past receipt.
  ///
  /// A target before receipt returns the base state: extrapolation predicts
  /// forward from the base, and the past is interpolation's job.
  ///
  /// The Rust signature returns an `Option` whose `None` no path produces, so this
  /// returns the state directly.
  S at(ClientTimeMs targetClientRenderTimeMs, int maxExtrapolationDurationMs) {
    if (targetClientRenderTimeMs < clientReceiptTimeMs) return state;

    final elapsedMs = targetClientRenderTimeMs - clientReceiptTimeMs;

    // Cap the *duration*, do not discard the extrapolation.
    //
    // Returning the un-extrapolated state past the limit is the obvious reading of
    // "clamp", and it is a discontinuity: at the limit the entity has coasted
    // `velocity * max_ms` forward, and one millisecond later it is drawn back at
    // the raw sample. That is a jump of the entire extrapolation window, in the
    // wrong direction, and jitter around the boundary makes it flicker back and
    // forth. Capping the duration instead means the entity coasts to the limit and
    // stops there, which is continuous.
    final cappedMs = elapsedMs < maxExtrapolationDurationMs ? elapsedMs : maxExtrapolationDurationMs;
    if (elapsedMs > maxExtrapolationDurationMs) overExtrapolations++;

    return extrapolateBy(state, velocity, cappedMs / 1000.0);
  }
}
