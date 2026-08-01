import 'saturating.dart';

/// Smoothed statistics over one stream's arrivals: the terms of the
/// render-delay budget, measured rather than configured.
///
/// A client cannot be told the send rate or the delay, and being told would be
/// worse anyway: a configured rate is wrong exactly when the server changes it,
/// which is when it matters.
///
/// Two decisions the Rust original records as learned the hard way. **The
/// buffer covers irregularity, not delay**, so the jitter term is the smoothed
/// mean deviation of lateness rather than the lateness itself: a steady 200ms
/// link needs no more buffer than a steady 20ms one. And **the interval is
/// measured between declared stamps, not arrivals**, because two packets can
/// arrive in one poll and still describe moments an interval apart.
///
/// Ported from `plaza_client_utils::arrival::ArrivalMonitor`, authoritative.
class ArrivalMonitor {
  /// [smoothing] is the EWMA weight for new observations, 0 to 1. Around 0.05
  /// follows a link's drift without chasing individual packets.
  ArrivalMonitor(double smoothing) : _smoothing = smoothing.clamp(0.0, 1.0);

  final double _smoothing;
  double _intervalMs = 0;
  int _newestStamp = 0;
  double _latenessMeanMs = 0;
  double _jitterMs = 0;

  /// Whether lateness has its first sample. A flag rather than a zero
  /// sentinel, because zero is a legitimate mean: a loopback client's lateness
  /// really is 0ms, and treating that as unseeded re-seeds on every packet and
  /// freezes the jitter at its initial value.
  bool _latenessSeeded = false;

  /// [stamp] is the declared server time a packet describes; [recv] is the
  /// client's synced estimate of server time at arrival.
  ///
  /// Call for every packet, reordered or not: a stamp older than the newest
  /// still updates lateness, because it *is* late and that is data, but never
  /// the interval, which is measured forward only.
  void observe(int stamp, int recv) {
    final lateness = saturatingSub(recv, stamp).toDouble();
    if (_newestStamp > 0 && stamp > _newestStamp) {
      final gap = (stamp - _newestStamp).toDouble();
      _intervalMs = _intervalMs == 0.0 ? gap : _intervalMs + (gap - _intervalMs) * _smoothing;
    }
    if (stamp > _newestStamp) _newestStamp = stamp;

    if (!_latenessSeeded) {
      _latenessSeeded = true;
      _latenessMeanMs = lateness;
    } else {
      final deviation = (lateness - _latenessMeanMs).abs();
      _latenessMeanMs += (lateness - _latenessMeanMs) * _smoothing;
      _jitterMs += (deviation - _jitterMs) * _smoothing;
    }
  }

  /// The smoothed gap between declared stamps: the send interval as it actually
  /// is, whatever the server was configured to.
  double get intervalMs => _intervalMs;

  /// The smoothed mean lateness: with an honest clock sync, the link's one-way
  /// delay plus whatever error the sync carries.
  double get latenessMs => _latenessMeanMs;

  /// The smoothed mean deviation of lateness: the irregularity the buffer
  /// exists to cover.
  double get jitterMs => _jitterMs;

  /// The render delay this stream needs: lateness, plus spread, plus one send
  /// interval. Whether to *adapt* to it is the application's decision, since a
  /// delay that follows the link hides bad links instead of reporting them.
  double get neededDelayMs => _latenessMeanMs + _jitterMs + _intervalMs;

  /// Whether at least two forward stamps have been seen, so an interval exists.
  bool get warmedUp => _intervalMs > 0.0;

  /// Forgets every measurement. For a resume, where the samples describe a link
  /// that was measured before an arbitrary gap.
  void reset() {
    _intervalMs = 0;
    _newestStamp = 0;
    _latenessMeanMs = 0;
    _jitterMs = 0;
    _latenessSeeded = false;
  }
}
