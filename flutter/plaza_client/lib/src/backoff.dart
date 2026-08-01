import 'dart:math';

/// How long to wait before trying again.
///
/// Exponential with a ceiling and jitter, which is roast_republic's reconnect
/// service generalised. The jitter matters more than the curve: without it a
/// server that drops every client at once gets them all back in the same
/// millisecond, which is how a recoverable blip becomes an outage.
class Backoff {
  Backoff({
    this.initial = const Duration(seconds: 1),
    this.factor = 1.8,
    this.ceiling = const Duration(seconds: 30),
    this.jitter = 0.2,
    this.maxAttempts,
    Random? random,
  })  : assert(factor >= 1, 'a factor below 1 shortens each wait'),
        assert(jitter >= 0 && jitter < 1, 'jitter is a fraction of the delay'),
        _random = random ?? Random();

  final Duration initial;
  final double factor;
  final Duration ceiling;

  /// Fraction either side of the computed delay, so 0.2 means plus or minus 20%.
  final double jitter;

  /// Give up after this many attempts. Null retries for ever, which is right
  /// for a game a player leaves open.
  final int? maxAttempts;

  final Random _random;

  /// Whether an [attempt] (zero-based) should happen at all.
  bool shouldRetry(int attempt) => maxAttempts == null || attempt < maxAttempts!;

  /// The wait before [attempt], zero-based, jitter included.
  Duration delayFor(int attempt) {
    final raw = initial.inMicroseconds * pow(factor, attempt);
    final capped = min(raw, ceiling.inMicroseconds.toDouble());
    final spread = capped * jitter;
    final withJitter = capped - spread + _random.nextDouble() * spread * 2;
    return Duration(microseconds: max(0, withJitter.round()));
  }
}
