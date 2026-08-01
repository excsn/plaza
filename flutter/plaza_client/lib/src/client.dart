import 'dart:async';

import 'package:plaza_wire/plaza_wire.dart';

import 'backoff.dart';
import 'socket.dart';
import 'timeline.dart';

/// Where the client is, as an application cares about it.
enum PlazaStatus { idle, connecting, open, reconnecting, closed }

/// Something worth telling the application about.
sealed class PlazaEvent {
  const PlazaEvent();
}

class Connected extends PlazaEvent {
  const Connected({required this.resumed});

  /// Whether this is a return rather than a first arrival. An application that
  /// needs a fresh snapshot after a gap asks for one here.
  final bool resumed;
}

class Disconnected extends PlazaEvent {
  const Disconnected(this.reason);
  final String reason;
}

/// The two ends were built from different wire definitions.
///
/// The browser client's answer is to reload. A shipped app cannot, so it has to
/// say so: this is the update prompt, and continuing past it means decoding
/// against a definition the server no longer holds.
class Outdated extends PlazaEvent {
  const Outdated({required this.ours, required this.theirs});
  final ProtocolVersion ours;
  final ProtocolVersion theirs;
}

/// Retries are finished and nothing further will be attempted.
class GaveUp extends PlazaEvent {
  const GaveUp(this.attempts);
  final int attempts;
}

/// A frame arrived whose kind this build does not know.
///
/// Skipped rather than fatal, and surfaced only so a diagnostic panel can count
/// them: a number that climbs means the server is ahead of this client.
class SkippedFrame extends PlazaEvent {
  const SkippedFrame(this.kindByte);
  final int kindByte;
}

/// A plaza connection: the handshake, the ops, and getting back after a drop.
///
/// Deliberately does not know what an op *is*. The Rust side defines the
/// vocabulary and this carries it, so ops arrive as decoded values and it is
/// the application that pattern-matches them. Use `variantName` from
/// `plaza_wire` rather than checking for a property, or every unit variant will
/// be silently dropped.
class PlazaClient {
  PlazaClient({
    required this.url,
    required SocketFactory connect,
    this.codec = const JsonCodec(),
    this.protocol = ProtocolVersion.unknown,
    Backoff? backoff,
    Timeline? timeline,
  })  : _connect = connect,
        _backoff = backoff ?? Backoff(),
        timeline = timeline ?? Timeline();

  final Uri url;
  final SocketFactory _connect;
  final WireCodec codec;

  /// This build's wire version. Generated alongside the wire types, never
  /// computed here: a Dart client cannot hash the Rust sources that define the
  /// format.
  final ProtocolVersion protocol;

  final Backoff _backoff;

  /// Clocks, and the epoch that says which measurements still count. Feed it
  /// with [Timeline.begin] and [Timeline.complete] around your own ping op:
  /// plaza has no ping of its own, since the transport's heartbeat is the
  /// server measuring the client.
  final Timeline timeline;

  final StreamController<Object?> _ops = StreamController<Object?>.broadcast();
  final StreamController<Pong> _pongs = StreamController<Pong>.broadcast();
  final StreamController<PlazaEvent> _events = StreamController<PlazaEvent>.broadcast();

  PlazaSocket? _socket;
  StreamSubscription<Object>? _sub;
  Timer? _retryTimer;
  int _attempt = 0;
  bool _stopped = false;
  bool _everConnected = false;

  PlazaStatus _status = PlazaStatus.idle;
  ProtocolVersion? _serverProtocol;

  /// Ops as they arrive, one event per op rather than one per frame, because a
  /// frame carrying three ops is an implementation detail of batching.
  Stream<Object?> get ops => _ops.stream;

  /// Answers to the probes [sendPing] started. Pair each with the [Probe] it
  /// returned and hand both to [Timeline.complete].
  Stream<Pong> get pongs => _pongs.stream;

  Stream<PlazaEvent> get events => _events.stream;

  PlazaStatus get status => _status;

  /// What the server said it speaks, once its Hello has arrived.
  ProtocolVersion? get serverProtocol => _serverProtocol;

  /// Whether the two ends agree, treating "not yet known" as agreement, the
  /// same rule the Rust side applies.
  bool get agreed => _serverProtocol == null || protocol.agreesWith(_serverProtocol!);

  /// Opens the connection and keeps it open.
  Future<void> start() async {
    _stopped = false;
    await _open();
  }

  /// Sends ops. Dropped if the socket is not open, because a queue that
  /// survives a reconnect replays intent the player has moved on from.
  bool sendOps(List<Object?> ops) {
    final socket = _socket;
    if (socket == null || socket.state != SocketState.open) return false;
    socket.send(buildFrame(Kind.ops, codec.encode(ops)));
    return true;
  }

  /// Sends one op.
  bool sendOp(Object? op) => sendOps(<Object?>[op]);

  /// Sends one frame of any kind, for the control plane an op enum has no
  /// business carrying. [sendPing] is the reason this exists.
  bool sendFrame(Kind kind, Object? body) {
    final socket = _socket;
    if (socket == null || socket.state != SocketState.open) return false;
    socket.send(buildFrame(kind, codec.encode(body)));
    return true;
  }

  /// Starts a latency probe, returning the [Probe] to complete when the answer
  /// arrives on [pongs].
  ///
  /// [nowMs] is your own clock and its unit is yours: it comes back echoed and
  /// the server never reads it.
  Probe? sendPing(int nowMs) {
    final probe = timeline.begin(nowMs);
    if (!sendFrame(Kind.ping, <String, Object?>{'origin': probe.sentAtMs})) {
      return null;
    }
    return probe;
  }

  /// Call on `AppLifecycleState.resumed`.
  ///
  /// A suspended app is the suspended browser tab problem wearing a different
  /// name. Whatever queued while the process was frozen describes a world that
  /// has moved on, so it is dropped unread rather than played out, and the
  /// connection is remade if it did not survive. The application learns this
  /// through [Connected] with `resumed` set, which is where it should ask for a
  /// fresh snapshot rather than trying to catch up.
  Future<void> resume() async {
    if (_stopped) return;
    // Arbitrary wall time passed, so anything in flight measures the suspend
    // and anything learned was learned before it.
    timeline.onResume();
    final socket = _socket;
    if (socket != null && socket.state == SocketState.open) {
      _events.add(const Connected(resumed: true));
      return;
    }
    _retryTimer?.cancel();
    _attempt = 0;
    await _open();
  }

  Future<void> stop() async {
    _stopped = true;
    _retryTimer?.cancel();
    _retryTimer = null;
    await _sub?.cancel();
    _sub = null;
    await _socket?.close();
    _socket = null;
    _setStatus(PlazaStatus.closed);
    await _ops.close();
    await _pongs.close();
    await _events.close();
  }

  void _setStatus(PlazaStatus s) => _status = s;

  Future<void> _open() async {
    if (_stopped) return;
    _setStatus(_everConnected ? PlazaStatus.reconnecting : PlazaStatus.connecting);

    final PlazaSocket socket;
    try {
      socket = await _connect(url);
    } catch (e) {
      _scheduleRetry('connect failed: $e');
      return;
    }
    if (_stopped) {
      await socket.close();
      return;
    }

    _socket = socket;
    _serverProtocol = null;
    if (_everConnected) timeline.onReconnect();
    _sub = socket.messages.listen(
      _onFrame,
      onDone: () => _onClosed('socket closed'),
      onError: (Object e) => _onClosed('socket error: $e'),
      cancelOnError: false,
    );

    // Ours goes first and unprompted: the Rust side does the same, so neither
    // end waits for the other and a peer built before the handshake existed
    // simply never answers.
    socket.send(buildFrame(Kind.hello, codec.encode(protocol.value)));

    final resumed = _everConnected;
    _everConnected = true;
    _attempt = 0;
    _setStatus(PlazaStatus.open);
    _events.add(Connected(resumed: resumed));
  }

  void _onFrame(Object message) {
    final frame = splitFrame(message);
    if (frame == null) return;

    final kind = frame.kind;
    if (kind == null) {
      _events.add(SkippedFrame(frame.kindByte));
      return;
    }

    switch (kind) {
      case Kind.hello:
        final value = codec.decode(frame.body);
        final theirs = ProtocolVersion(value is int ? value : 0);
        _serverProtocol = theirs;
        if (!protocol.agreesWith(theirs)) {
          _events.add(Outdated(ours: protocol, theirs: theirs));
        }
      // Answered here rather than surfaced, because echoing a value back is
      // something this client can finish by itself. The server's session is
      // timing the link and this is the half it cannot do alone.
      case Kind.ping:
        final body = codec.decode(frame.body);
        if (body is Map) {
          sendFrame(Kind.pong, <String, Object?>{
            'origin': body['origin'],
            'responder': null,
          });
        } else if (body is List && body.isNotEmpty) {
          sendFrame(Kind.pong, <Object?>[body.first, null]);
        }
      case Kind.pong:
        final body = codec.decode(frame.body);
        final origin = body is Map ? body['origin'] : (body is List && body.isNotEmpty ? body[0] : null);
        final responder = body is Map ? body['responder'] : (body is List && body.length > 1 ? body[1] : null);
        if (origin is int) {
          _pongs.add(Pong(origin, responder is num ? responder.toDouble() : null));
        }
      case Kind.ops:
        final decoded = codec.decode(frame.body);
        if (decoded is List) {
          for (final op in decoded) {
            _ops.add(op);
          }
        } else if (decoded != null) {
          // A body that is not a list is a codec or shape disagreement, not a
          // one-op batch. Surfacing it beats silently treating it as an op.
          _events.add(Disconnected('ops frame was ${decoded.runtimeType}, expected a list'));
        }
    }
  }

  void _onClosed(String reason) {
    _sub?.cancel();
    _sub = null;
    _socket = null;
    if (_stopped) return;
    _events.add(Disconnected(reason));
    _scheduleRetry(reason);
  }

  void _scheduleRetry(String reason) {
    if (_stopped) return;
    if (!_backoff.shouldRetry(_attempt)) {
      _setStatus(PlazaStatus.closed);
      _events.add(GaveUp(_attempt));
      return;
    }
    final delay = _backoff.delayFor(_attempt);
    _attempt++;
    _setStatus(PlazaStatus.reconnecting);
    _retryTimer?.cancel();
    _retryTimer = Timer(delay, () {
      unawaited(_open());
    });
  }
}
