import 'arrival.dart';
import 'interpolation.dart';

/// The render clock and the measurements that size it.
///
/// A game loop has the one thing a client library does not: a `dt` every frame.
/// This joins them, so the render target advances with the loop rather than with
/// packet arrivals. Seconds because that is what every loop hands out, not
/// because of any particular engine: nothing here knows what is driving it.
///
/// The delay is not adapted automatically. A delay that follows the link hides
/// bad links instead of reporting them, so [neededDelayMs] is a reading and
/// moving the delay stays the game's decision.
class RenderTimeline {
  RenderTimeline({int delayMs = 100, double smoothing = 0.05})
      : clock = InterpolationClock(delayMs),
        arrival = ArrivalMonitor(smoothing);

  final InterpolationClock clock;
  final ArrivalMonitor arrival;

  /// [stampMs] is the server time the packet describes, [recvMs] the client's
  /// estimate of server time when it arrived.
  void observe(int stampMs, int recvMs) {
    arrival.observe(stampMs, recvMs);
    clock.observe(stampMs);
  }

  /// Steers the estimate toward the newest stamp instead of free-running.
  void resync(int newestStampMs, [double strength = 0.1]) =>
      clock.resync(newestStampMs, strength);

  /// Advances the target by one frame's worth of seconds.
  void advance(double dtSeconds) => clock.advance((dtSeconds * 1000).round());

  /// For a clock steered by [InterpolationClock.observeRate].
  void advanceScaled(double dtSeconds) => clock.advanceScaled((dtSeconds * 1000).round());

  /// Where on the server timeline to draw, or null before the first packet.
  int? get target => clock.target;

  int get delayMs => clock.delay;

  set delayMs(int value) => clock.delay = value;

  /// What the measured stream says the delay should be.
  double get neededDelayMs => arrival.neededDelayMs;

  /// Whether the delay in force is shorter than the stream needs, which is the
  /// condition that starves interpolation.
  bool get underBudget => arrival.warmedUp && arrival.neededDelayMs > clock.delay;

  /// Drops the measurements and un-starts the clock, keeping the delay.
  ///
  /// For a resume: the samples describe a link measured before an arbitrary
  /// gap, and the estimate is stale by however long the app was suspended.
  void reset() {
    clock.reset();
    arrival.reset();
  }
}
