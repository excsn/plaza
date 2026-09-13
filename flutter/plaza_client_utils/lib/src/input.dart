/// Decides whether this frame's input needs to go on the wire.
///
/// Against a server that holds an input and integrates it every tick, sending
/// the same direction sixty times a second tells it nothing new.
///
/// Transmission and integration are separate decisions, which makes
/// coalescing safe: local prediction
/// advances every tick whatever the wire is doing, so the player does not
/// stutter while nothing is sent. It also means this pairs with a held-input
/// server and **not** with one that consumes one input per step, where dropping
/// repeats drops actual movement.
///
/// Ported from `plaza_client_utils::coalesce::InputCoalescer`.
class InputCoalescer<I> {
  /// Resends the held input at least every [keepaliveMs].
  ///
  /// Pick the interval by how long a wrong direction is tolerable rather than by
  /// bandwidth: it is the longest a dropped change persists.
  InputCoalescer(this.keepaliveMs);

  final int keepaliveMs;
  I? _lastSent;
  int _lastSentMs = 0;
  bool _seeded = false;
  bool enabled = true;

  /// Whether to transmit [input] now.
  ///
  /// The keepalive is not optional. Sending purely on change fails under loss:
  /// the server holds the last direction it received, so a *dropped* change
  /// leaves it holding a wrong state until the player presses something else.
  /// It reads as the controls sticking and looks
  /// nothing like packet loss.
  bool shouldSend(I input, int nowMs) {
    if (!enabled) return true;
    if (!_seeded) {
      _seeded = true;
      _lastSent = input;
      _lastSentMs = nowMs;
      return true;
    }
    final changed = _lastSent != input;
    final stale = nowMs - _lastSentMs >= keepaliveMs;
    if (changed || stale) {
      _lastSent = input;
      _lastSentMs = nowMs;
      return true;
    }
    return false;
  }

  I? get lastSent => _lastSent;

  void reset() {
    _seeded = false;
    _lastSent = null;
    _lastSentMs = 0;
  }
}

/// Names the tick an input is meant for, floored by the newest stamp the stream
/// has delivered.
///
/// The clock names the tick; the newest arrived stamp bounds it from below. The
/// server wrote that stamp, so server time is provably past it. An input aimed
/// behind it is certain to be rejected.
///
/// This matters most after a resume, when a clock fit can trail the stream by
/// hundreds of milliseconds while its window refills. Measured in horde: aiming
/// five ticks behind a four-tick accepting window dropped every input. The floor
/// keeps them inside the window with no clock involved at all.
///
/// It only ever lifts the aim, and never past the ideal: a stamp trails true
/// server time by the one-way delay, so `stamp + depth` is at most where a
/// perfect clock would have aimed.
///
/// Extracted from `horde_playground`'s client, where this rule currently lives
/// inline rather than in the Rust crate.
class TickNamer {
  TickNamer({required this.stepMs, this.playoutDelayMs = 0}) {
    if (stepMs <= 0) {
      throw ArgumentError.value(stepMs, 'stepMs', 'must be positive');
    }
  }

  /// The server's simulation step, in milliseconds.
  final int stepMs;

  /// How far ahead the server accepts inputs for.
  int playoutDelayMs;

  int _newestStampMs = 0;

  /// Records the newest server stamp seen. Only ever moves forward.
  void observeStamp(int stampMs) {
    if (stampMs > _newestStampMs) _newestStampMs = stampMs;
  }

  int get newestStampMs => _newestStampMs;

  /// The tick to name, given the clock's estimate of server time now.
  ///
  /// This is only a request: the server decides whether that tick is open.
  int tickFor(int serverNowMs) {
    final aimed = (serverNowMs + playoutDelayMs) ~/ stepMs;
    final floor = (_newestStampMs + playoutDelayMs) ~/ stepMs;
    return aimed > floor ? aimed : floor;
  }

  /// Whether the floor is currently raising the aim, which means the clock is
  /// trailing the stream.
  bool floorApplies(int serverNowMs) =>
      (_newestStampMs + playoutDelayMs) ~/ stepMs > (serverNowMs + playoutDelayMs) ~/ stepMs;

  void reset() => _newestStampMs = 0;
}
