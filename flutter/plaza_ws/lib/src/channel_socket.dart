import 'dart:async';

import 'package:plaza_client/plaza_client.dart';
import 'package:web_socket_channel/web_socket_channel.dart';

/// A [PlazaSocket] over `web_socket_channel`.
///
/// Separate from `plaza_client` so that package stays dependency-free and has
/// nothing to conditionally import. `web_socket_channel` picks `dart:io` or the
/// browser implementation itself, so this reaches every target plaza supports.
class ChannelSocket implements PlazaSocket {
  ChannelSocket._(this._channel) {
    _sub = _channel.stream.listen(
      (message) {
        if (message is String || message is List<int>) {
          _incoming.add(message as Object);
        }
      },
      onError: (Object error) => _finish(),
      onDone: _finish,
      cancelOnError: false,
    );
    unawaited(_channel.sink.done.then((_) => _finish()).catchError((_) => _finish()));
  }

  /// Connects, and does not return until the handshake has completed.
  ///
  /// Waiting matters: `WebSocketChannel.connect` returns immediately and a frame
  /// sent before the socket is open is dropped silently on some platforms, which
  /// would lose the Hello.
  static Future<ChannelSocket> connect(Uri url, {Iterable<String>? protocols}) async {
    final channel = WebSocketChannel.connect(url, protocols: protocols);
    await channel.ready;
    return ChannelSocket._(channel);
  }

  /// Wraps a channel you already have, for a server or a test harness.
  static ChannelSocket wrap(WebSocketChannel channel) => ChannelSocket._(channel);

  final WebSocketChannel _channel;
  final StreamController<Object> _incoming = StreamController<Object>();
  final Completer<void> _done = Completer<void>();
  StreamSubscription<dynamic>? _sub;
  SocketState _state = SocketState.open;

  void _finish() {
    if (_state == SocketState.closed) return;
    _state = SocketState.closed;
    unawaited(_sub?.cancel());
    _sub = null;
    if (!_incoming.isClosed) _incoming.close();
    if (!_done.isCompleted) _done.complete();
  }

  @override
  Stream<Object> get messages => _incoming.stream;

  @override
  void send(Object frame) {
    if (_state != SocketState.open) return;
    _channel.sink.add(frame);
  }

  @override
  SocketState get state => _state;

  @override
  Future<void> get done => _done.future;

  @override
  Future<void> close() async {
    if (_state == SocketState.closed) return;
    // `_finish` is the only thing that moves the state, or it would see the
    // socket as already closed and leave `done` hanging for ever.
    final sink = _channel.sink;
    _finish();
    await sink.close();
  }
}

/// A [SocketFactory] for [PlazaClient], usable as-is:
///
/// ```dart
/// final client = PlazaClient(url: uri, connect: webSocketConnect);
/// ```
Future<PlazaSocket> webSocketConnect(Uri url) => ChannelSocket.connect(url);
