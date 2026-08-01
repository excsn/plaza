import 'package:flutter/foundation.dart';
import 'package:plaza_client/plaza_client.dart';

/// What a connection has actually done, for a panel to show.
///
/// In the tradition of the playgrounds' readouts: every number here exists
/// because a fault was invisible without it. `framesSkipped` climbing means the
/// server is ahead of this build; `reconnects` climbing on a still-connected
/// session means the link is flapping rather than down, which looks identical
/// from inside the game and is a different problem.
class PlazaStats extends ChangeNotifier {
  PlazaStatus status = PlazaStatus.idle;
  int opsIn = 0;
  int opsOut = 0;
  int reconnects = 0;
  int framesSkipped = 0;
  int resumes = 0;

  /// Set when the two ends were built from different wire definitions. An app
  /// showing this should be prompting for an update, not playing on.
  Outdated? outdated;

  /// Set when reconnection gave up.
  GaveUp? gaveUp;

  String? lastDisconnectReason;

  bool get healthy => status == PlazaStatus.open && outdated == null;

  void reset() {
    status = PlazaStatus.idle;
    opsIn = 0;
    opsOut = 0;
    reconnects = 0;
    framesSkipped = 0;
    resumes = 0;
    outdated = null;
    gaveUp = null;
    lastDisconnectReason = null;
    notifyListeners();
  }

  /// Folds one event in. Called by the mixin; an app rarely calls it directly.
  void apply(PlazaEvent event, PlazaStatus current) {
    status = current;
    switch (event) {
      case Connected(:final resumed):
        if (resumed) {
          reconnects++;
          resumes++;
        }
      case Disconnected(:final reason):
        lastDisconnectReason = reason;
      case Outdated():
        outdated = event;
      case GaveUp():
        gaveUp = event;
      case SkippedFrame():
        framesSkipped++;
    }
    notifyListeners();
  }

  void countIn() {
    opsIn++;
    notifyListeners();
  }

  void countOut(int n) {
    opsOut += n;
    notifyListeners();
  }
}
