import 'package:plaza_client_utils/plaza_client_utils.dart';

/// An answered probe: the stamp it went out with, echoed back untouched, and
/// the responder's clock if it had one to offer.
class Pong {
  const Pong(this.origin, this.responderMs);
  final int origin;
  final double? responderMs;
}

/// A latency measurement in flight.
///
/// Carries the epoch it was started in. A probe whose epoch has moved on is
/// discarded rather than recorded: a ping sent before the app was suspended and
/// answered after it measures the suspend, not the network, and one such sample
/// poisons a smoothed estimator for minutes.
class Probe {
  const Probe(this.epoch, this.sentAtMs);
  final int epoch;
  final int sentAtMs;
}

/// The client's clocks, and the epoch that says which measurements still count.
///
/// A **reconnect** invalidates measurements in flight but keeps what has been
/// learned: the socket changed, the link probably did not. A **resume**
/// invalidates both, because arbitrary wall time passed and a least-squares fit
/// across a ten-minute gap produces a meaningless skew.
class Timeline {
  Timeline({RttEstimator? rtt, ClockSyncEstimator? clock})
      : rtt = rtt ?? RttEstimator(),
        clock = clock ?? ClockSyncEstimator(32);

  final RttEstimator rtt;
  final ClockSyncEstimator clock;

  int _epoch = 0;
  int get epoch => _epoch;

  /// Starts a measurement. Send your ping op stamped with [nowMs].
  Probe begin(int nowMs) => Probe(_epoch, nowMs);

  /// Records a completed exchange. Returns false if the probe was discarded.
  ///
  /// [serverTimeMs] is the server clock stamped in the reply; pass null to feed
  /// the round trip only.
  bool complete(Probe probe, int nowMs, {double? serverTimeMs}) {
    if (probe.epoch != _epoch) return false;
    rtt.observePong(probe.sentAtMs, nowMs);
    if (serverTimeMs != null) {
      clock.observeExchange(probe.sentAtMs.toDouble(), serverTimeMs, nowMs.toDouble());
    }
    return true;
  }

  /// Discards measurements in flight, keeping what is already learned.
  void onReconnect() => _epoch++;

  /// Discards measurements in flight and everything learned.
  void onResume() {
    _epoch++;
    rtt.clear();
    clock.clear();
  }
}
