/// What [PlayoutBuffer.push] concluded about the packet it was handed.
enum Admission {
  /// Queued for its instant. Nothing to do until the clock reaches it.
  queued,

  /// The gap between this arrival and the render instant is a discontinuity, not
  /// a delay. The buffer has already dropped everything but the newest packet;
  /// the caller must now restart its own timeline: re-anchor its render clock on
  /// what just arrived and drop derived state, its entity mirror above all, so
  /// the stream's own recovery rebuilds it.
  timelineLost,
}

/// The playout queue: push on arrival, pop what is due at the render instant.
///
/// `stamp` is the instant a packet describes, in the application's units;
/// `order` is its sequence number, which is what playout is ordered by, so
/// deltas compose in the order the server built them even when arrivals
/// interleave. The two advance together on any real stream; this only assumes
/// they agree about which packet is newest.
///
/// Ported from `plaza_client_utils::playout::PlayoutBuffer`.
class PlayoutBuffer<T> {
  /// [maxQueued] bounds the queue absolutely: size it several times past what an
  /// honest buffer holds at the deepest render delay and fastest send rate, so
  /// reaching it means something is wrong rather than merely slow.
  ///
  /// [lostAhead] is the discontinuity threshold: how far past the render instant
  /// an arrival may reach before the client is lost rather than buffering. Match
  /// it with the server's stalled-subscriber threshold, so both sides agree on
  /// when a gap stops being jitter.
  PlayoutBuffer({required int maxQueued, required this.lostAhead})
      : maxQueued = maxQueued < 1 ? 1 : maxQueued;

  final int maxQueued;
  final int lostAhead;

  /// Sorted by order, so popping is always the oldest surviving packet.
  final List<(int, int, T)> _queued = <(int, int, T)>[];
  int _underruns = 0;
  int _restarts = 0;

  /// Takes delivery of a packet. [renderAt] is the instant currently being drawn,
  /// null before the timeline has started, during which nothing can be late and
  /// nothing can be a discontinuity.
  Admission push(int stamp, int order, T item, int? renderAt) {
    if (renderAt != null) {
      final lateBy = renderAt > stamp ? renderAt - stamp : 0;
      if (lateBy > 0 && lateBy < lostAhead) _underruns++;
    }

    var i = 0;
    while (i < _queued.length && _queued[i].$2 <= order) {
      i++;
    }
    _queued.insert(i, (stamp, order, item));

    final ahead = renderAt == null ? 0 : (stamp > renderAt ? stamp - renderAt : 0);
    if (ahead > lostAhead || _queued.length > maxQueued) {
      _restart();
      return Admission.timelineLost;
    }
    return Admission.queued;
  }

  /// The oldest packet whose instant the clock has reached, in sequence order.
  /// Call in a loop each tick until it returns null.
  T? popDue(int renderAt) {
    if (_queued.isNotEmpty && _queued.first.$1 <= renderAt) {
      return _queued.removeAt(0).$3;
    }
    return null;
  }

  /// The transport's verdict that the timeline is lost, arriving from outside: a
  /// resume backlog discarded unread, a reconnect. Drops everything but the
  /// newest, which is what the caller's restarted clock anchors on.
  void timelineLost() => _restart();

  void _restart() {
    _restarts++;
    // Keep only the newest: it is what the clock is about to anchor on, and
    // everything older describes moments now past.
    final newest = _queued.isEmpty ? null : _queued.removeLast();
    _queued.clear();
    if (newest != null) _queued.add(newest);
  }

  /// Packets that arrived after the instant they describe had been drawn, by a
  /// margin jitter produces. The number that says the render delay is too small
  /// for this link.
  int get underruns => _underruns;

  /// How many stalls were survived. Counted per restart rather than per packet
  /// dropped, so it does not scale with how large a given backlog happened to be.
  int get restarts => _restarts;

  Iterable<T> get items => _queued.map((e) => e.$3);
  int get length => _queued.length;
  bool get isEmpty => _queued.isEmpty;
}
