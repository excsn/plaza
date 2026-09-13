import 'dart:async';

/// Where a socket is in its life. Monotonic: nothing returns to [connecting].
enum SocketState { connecting, open, closed }

/// The transport, as this package needs it.
///
/// Not tied to WebSocket. Reaching every Dart target with one socket
/// implementation means `dart:io` on native and `package:web` on the browser
/// and picking either here would decide the app's platform support for it.
/// Supply your own (`web_socket_channel` is the usual answer) and this package
/// stays pure Dart with nothing to conditionally import.
abstract class PlazaSocket {
  /// Frames as they arrive: a `String` for a text frame, a `List<int>` for a
  /// binary one. Which arrives follows the server's codec.
  ///
  /// **Single-subscription and it must buffer whatever arrives before the first
  /// listener.** The server speaks first, so a socket is normally open before
  /// anyone is listening and a broadcast stream discards those frames without a
  /// trace. The `Hello` is the first thing on the wire and therefore the first
  /// thing lost.
  Stream<Object> get messages;

  /// Sends one frame, already built. Same two shapes.
  void send(Object frame);

  SocketState get state;

  /// Completes when the socket is finished, however it finished.
  Future<void> get done;

  /// The WebSocket close code once the far side has sent one. `null` before
  /// that or when the link died without a close frame.
  ///
  /// This is how a rejection is told from a drop. RFC 6455 reserves 4000-4999
  /// for the application, so a server refusing a credential closes with a
  /// 4xxx; 1006 is no close frame at all, which is the transport failing rather
  /// than the server deciding. A client that cannot tell them apart either
  /// retries a token the server has already refused or gives up on a
  /// connection that only needed reconnecting.
  int? get closeCode;

  Future<void> close();
}

/// Opens a socket to [url]. Called again on every reconnect, so it must be
/// usable more than once.
typedef SocketFactory = Future<PlazaSocket> Function(Uri url);

/// A socket pair with no network, for tests and local play.
///
/// Mirrors the `loopback` feature of the Rust `plaza_ws` crate, so the
/// lifecycle can be tested without a server.
class LoopbackSocket implements PlazaSocket {
  LoopbackSocket() : _incoming = StreamController<Object>();

  final StreamController<Object> _incoming;
  final List<Object> sent = <Object>[];
  final Completer<void> _done = Completer<void>();
  SocketState _state = SocketState.open;
  int? _closeCode;

  /// Frames the client has sent, as the other end would see them.
  Object? get lastSent => sent.isEmpty ? null : sent.last;

  /// Delivers a frame to the client as though the server had sent it.
  void deliver(Object frame) {
    if (_state == SocketState.closed) return;
    _incoming.add(frame);
  }

  /// Ends the connection from the far side, which is what a drop looks like.
  ///
  /// [code] is the close code the server sent; leaving it out is a link that
  /// died without one.
  void dropFromServer({int? code}) {
    if (_state == SocketState.closed) return;
    _state = SocketState.closed;
    _closeCode = code;
    _incoming.close();
    if (!_done.isCompleted) _done.complete();
  }

  @override
  Stream<Object> get messages => _incoming.stream;

  @override
  void send(Object frame) {
    if (_state != SocketState.open) return;
    sent.add(frame);
  }

  @override
  SocketState get state => _state;

  @override
  Future<void> get done => _done.future;

  @override
  int? get closeCode => _closeCode;

  @override
  Future<void> close() async {
    dropFromServer();
  }
}
