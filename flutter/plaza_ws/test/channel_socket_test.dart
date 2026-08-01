import 'dart:async';
import 'dart:io';

import 'package:plaza_ws/plaza_ws.dart';
import 'package:test/test.dart';
import 'package:web_socket_channel/io.dart';

/// A real WebSocket server, so the adapter is tested against a socket rather
/// than against another fake. No plaza involved: this is the transport only.
class EchoServer {
  EchoServer._(this._server, this.uri);

  final HttpServer _server;
  final Uri uri;
  final List<Object> received = <Object>[];
  WebSocket? _peer;

  static Future<EchoServer> start() async {
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    final s = EchoServer._(server, Uri.parse('ws://127.0.0.1:${server.port}'));
    unawaited(s._accept());
    return s;
  }

  Future<void> _accept() async {
    await for (final request in _server) {
      final socket = await WebSocketTransformer.upgrade(request);
      _peer = socket;
      socket.listen(
        (dynamic m) => received.add(m as Object),
        onDone: () => _peer = null,
        cancelOnError: false,
      );
    }
  }

  void send(Object frame) => _peer?.add(frame);

  Future<void> hangUp() async => _peer?.close();

  Future<void> stop() async => _server.close(force: true);
}

Future<void> pump([int ms = 60]) => Future<void>.delayed(Duration(milliseconds: ms));

void main() {
  late EchoServer server;

  setUp(() async => server = await EchoServer.start());
  tearDown(() async => server.stop());

  test('connect waits for the handshake before returning', () async {
    final socket = await ChannelSocket.connect(server.uri);
    expect(socket.state, SocketState.open);
    // The point of waiting: a frame sent here must not be dropped.
    socket.send('first');
    await pump();
    expect(server.received, ['first']);
    await socket.close();
  });

  test('a text frame arrives as a String', () async {
    final socket = await ChannelSocket.connect(server.uri);
    final seen = <Object>[];
    socket.messages.listen(seen.add);
    server.send('hello');
    await pump();
    expect(seen.single, 'hello');
    await socket.close();
  });

  /// The server speaks first, so a frame that arrives before anyone is listening
  /// is the normal case. This was a broadcast controller, which drops events with
  /// no listener, and what it dropped was the very first frame on the wire: the
  /// server's `Hello`. The symptom was a handshake that looked like it had never
  /// happened, on a connection that was working.
  test('a frame that arrives before the first listener is not lost', () async {
    final socket = await ChannelSocket.connect(server.uri);
    server.send('spoken first');
    await pump();

    final seen = <Object>[];
    socket.messages.listen(seen.add);
    await pump();

    expect(seen, ['spoken first']);
    await socket.close();
  });

  test('a binary frame arrives as bytes', () async {
    final socket = await ChannelSocket.connect(server.uri);
    final seen = <Object>[];
    socket.messages.listen(seen.add);
    server.send(<int>[0, 1, 2, 3]);
    await pump();
    expect(seen.single, isA<List<int>>());
    expect(seen.single, <int>[0, 1, 2, 3]);
    await socket.close();
  });

  test('framing survives a round trip both ways', () async {
    final socket = await ChannelSocket.connect(server.uri);
    final seen = <Object>[];
    socket.messages.listen(seen.add);

    socket.send(buildFrame(Kind.hello, const JsonCodec().encode(7)));
    await pump();
    final out = splitFrame(server.received.single)!;
    expect(out.kind, Kind.hello);
    expect(const JsonCodec().decode(out.body), 7);

    server.send(buildFrame(Kind.ops, const JsonCodec().encode(['Ping'])) as String);
    await pump();
    final back = splitFrame(seen.single)!;
    expect(back.kind, Kind.ops);
    expect(const JsonCodec().decode(back.body), ['Ping']);
    await socket.close();
  });

  test('the server hanging up closes the socket and completes done', () async {
    final socket = await ChannelSocket.connect(server.uri);
    var finished = false;
    unawaited(socket.done.then((_) => finished = true));

    await server.hangUp();
    await pump();

    expect(socket.state, SocketState.closed);
    expect(finished, isTrue);
  });

  test('closing locally completes done and stops sends', () async {
    final socket = await ChannelSocket.connect(server.uri);
    await socket.close();
    expect(socket.state, SocketState.closed);
    await socket.done;

    socket.send('after close');
    await pump();
    expect(server.received, isEmpty);
  });

  test('closing twice is harmless', () async {
    final socket = await ChannelSocket.connect(server.uri);
    await socket.close();
    await socket.close();
    expect(socket.state, SocketState.closed);
  });

  test('a refused connection reports rather than hanging', () async {
    final dead = Uri.parse('ws://127.0.0.1:1');
    await expectLater(ChannelSocket.connect(dead), throwsA(isA<Object>()));
  });

  test('a PlazaClient drives a real socket end to end', () async {
    final client = PlazaClient(
      url: server.uri,
      connect: webSocketConnect,
      protocol: const ProtocolVersion(11),
    );
    final ops = <Object?>[];
    client.ops.listen(ops.add);
    await client.start();
    await pump();

    // The client greets unprompted, so the server has its Hello already.
    final hello = splitFrame(server.received.single)!;
    expect(hello.kind, Kind.hello);
    expect(const JsonCodec().decode(hello.body), 11);

    server.send(buildFrame(Kind.ops, const JsonCodec().encode(['Ready'])) as String);
    await pump();
    expect(variantName(ops.single), 'Ready');
    await client.stop();
  });

  test('wrap adopts a channel that is already open', () async {
    final raw = IOWebSocketChannel.connect(server.uri);
    await raw.ready;
    final socket = ChannelSocket.wrap(raw);
    socket.send('wrapped');
    await pump();
    expect(server.received, ['wrapped']);
    await socket.close();
  });
}
