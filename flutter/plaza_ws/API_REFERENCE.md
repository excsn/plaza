# API Reference: `plaza_ws` (Dart)

## 1. Introduction & Core Concepts

`plaza_ws` is one implementation of [`PlazaSocket`](../plaza_client/API_REFERENCE.md#abstract-class-plazasocket), over `web_socket_channel`. That package picks `dart:io` or the browser implementation itself, so this reaches every target plaza supports.

It is separate from [`plaza_client`](../plaza_client/) so that package stays dependency-free with nothing to conditionally import.

```dart
import 'package:plaza_ws/plaza_ws.dart';
```

That entry point re-exports the whole of `plaza_client`, which in turn re-exports [`plaza_wire`](../plaza_wire/API_REFERENCE.md). Two names are its own: [`ChannelSocket`](#class-channelsocket) and [`webSocketConnect`](#function-websocketconnect).

## 2. Error Handling

[`ChannelSocket.connect`](#static-method-connect) throws whatever `web_socket_channel` throws when a connection cannot be established, which is a `WebSocketChannelException` or a platform socket exception. That is the one place this package throws.

[`PlazaClient`](../plaza_client/API_REFERENCE.md#class-plazaclient) catches it: a factory that throws produces a [`Disconnected`](../plaza_client/API_REFERENCE.md#class-disconnected) event and a scheduled retry, not an exception out of `start`.

After a socket is open, nothing throws. A stream error and a clean close are the same thing to a client that is going to reconnect either way, so both close [`messages`](../plaza_client/API_REFERENCE.md#property-messages) and complete [`done`](../plaza_client/API_REFERENCE.md#property-done). Sending on a closed socket is a silent no-op, because [`PlazaClient.sendOps`](../plaza_client/API_REFERENCE.md#method-sendops) has already checked [`state`](../plaza_client/API_REFERENCE.md#enum-socketstate) and returned false to its caller.

## 3. Core API

### Function `webSocketConnect`

```dart
Future<PlazaSocket> webSocketConnect(Uri url)
```

A [`SocketFactory`](../plaza_client/API_REFERENCE.md#typedef-socketfactory) usable as-is:

```dart
final client = PlazaClient(url: uri, connect: webSocketConnect);
```

Equivalent to `ChannelSocket.connect(url)` with no subprotocols.

### Class `ChannelSocket`

```dart
class ChannelSocket implements PlazaSocket {
  static Future<ChannelSocket> connect(Uri url, {Iterable<String>? protocols});
  static ChannelSocket wrap(WebSocketChannel channel);
}
```

A [`PlazaSocket`](../plaza_client/API_REFERENCE.md#abstract-class-plazasocket) over a `WebSocketChannel`. No public constructor; use one of the two statics.

[`messages`](../plaza_client/API_REFERENCE.md#property-messages) is **single-subscription**, as the contract requires, so frames that arrive before the first listener are buffered rather than dropped. The server speaks first, so this is what keeps the `Hello` from being lost.

Anything on the channel that is neither a `String` nor a `List<int>` is discarded rather than forwarded.

#### Static method `connect`

```dart
static Future<ChannelSocket> connect(Uri url, {Iterable<String>? protocols})
```

Connects, and **does not return until the handshake has completed**.

Waiting matters: `WebSocketChannel.connect` returns immediately, and a frame sent before the socket is open is dropped silently on some platforms, which would lose the `Hello`.

`protocols` is passed through as the WebSocket subprotocol list.

#### Static method `wrap`

```dart
static ChannelSocket wrap(WebSocketChannel channel)
```

Wraps a channel you already have, for a server-side harness or a test that supplies its own pair. Assumes the channel is already open; nothing is awaited.

#### Inherited surface

[`send`](../plaza_client/API_REFERENCE.md#method-send), [`state`](../plaza_client/API_REFERENCE.md#property-state), [`done`](../plaza_client/API_REFERENCE.md#property-done) and [`close`](../plaza_client/API_REFERENCE.md#method-close) behave as `PlazaSocket` specifies. `state` starts at `SocketState.open`, because `connect` has already waited for it, and moves to `closed` once, on the first of a stream error, a stream close, a sink close, or [`close`](../plaza_client/API_REFERENCE.md#method-close).
