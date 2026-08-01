# `plaza_ws` (Dart)

**License:** Mozilla Public License 2.0 (MPL-2.0) · **Status:** Experimental

A WebSocket [`PlazaSocket`](../plaza_client/API_REFERENCE.md#abstract-class-plazasocket) over `web_socket_channel`.

Full surface in [API_REFERENCE.md](API_REFERENCE.md).

## Install

```yaml
dependencies:
  plaza_ws:
    path: ../plaza_ws
```

It re-exports the whole of [`plaza_client`](../plaza_client/), which re-exports [`plaza_wire`](../plaza_wire/), so this is the only import a client needs.

## Usage

```dart
import 'package:plaza_ws/plaza_ws.dart';

final client = PlazaClient(
  url: Uri.parse('ws://127.0.0.1:8090/ws/lobby'),
  connect: webSocketConnect,
  codec: const MsgPackCodec(),
  protocol: const ProtocolVersion(3152889444),
);
await client.start();
```

`webSocketConnect` is a [`SocketFactory`](../plaza_client/API_REFERENCE.md#typedef-socketfactory) usable as-is. For subprotocols or a channel you already hold, use [`ChannelSocket.connect`](API_REFERENCE.md#static-method-connect) or [`ChannelSocket.wrap`](API_REFERENCE.md#static-method-wrap).

## It waits for the handshake

`ChannelSocket.connect` awaits `WebSocketChannel.ready` before returning. `WebSocketChannel.connect` on its own returns immediately, and a frame sent before the socket is open is dropped silently on some platforms, which would lose the `Hello`.

`web_socket_channel` picks `dart:io` or the browser implementation itself, so this reaches every target plaza supports without a conditional import.

## The example

[`example/lobby_client.dart`](example/lobby_client.dart) is a console client for `examples/lobby_world`: it connects over a real socket, prints the arenas, joins the quick-match queue and leaves it again. One file, no Flutter, no platform directories.

```sh
cd ../../examples && cargo run -p plaza_example_lobby_world &
dart run example/lobby_client.dart                # declares nothing, plays
dart run example/lobby_client.dart --protocol 1   # declares a wrong version
```

Exit codes are 0 when it played, 2 on a version skew, and 1 when the server was unreachable. `../e2e.sh` runs both invocations against the live server and asserts them.

The skew run is the one worth watching, because the ops keep arriving *after* the warning. Plaza recorded the disagreement and kept serving; the client is the thing that decided to stop. The example picks that policy and says why: a console client cannot reload itself, and playing on would corrupt state other players can see. It also names the answer that is always wrong, which is retrying, since the next connection reaches the same server with the same two versions.
