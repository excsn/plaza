# API Reference: `plaza_wire`

## 1. Introduction & Core Concepts

`plaza_wire` holds the encoding contract shared by a Plaza server and any client that speaks to it: one trait and one implementation, with no async dependencies.

It is separate from `plaza_session` so that both ends of a connection can agree on a format without the client inheriting the server's runtime. A wasm or browser-targeted client depends on this crate alone; a server gets the same items re-exported from `plaza_session`, as `plaza_session::WireCodec` and `plaza_session::codec::WireCodec`.

Everything a transport sends or receives passes through a codec, so choosing a format is a one-line change that touches no transport code.

## 2. Error Handling

This crate defines no error type. Both trait methods return `Box<dyn std::error::Error + Send + Sync>`, so an implementation propagates whatever its underlying library produces (`serde_json::Error`, `rmp_serde::encode::Error`) without a conversion layer. `plaza_session` wraps these into its own `SessionLayerError::Serialization` and `::Deserialization` variants, tagging them with [`name`](#method-name) so a log line says which format failed.

## 3. Core API

### Trait `WireCodec`

```rust
pub trait WireCodec: Clone + Send + Sync + 'static {
  fn name(&self) -> &'static str;
  fn encode<T: Serialize>(&self, value: &T) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>>;
  fn decode<T: DeserializeOwned>(&self, bytes: &[u8]) -> Result<T, Box<dyn std::error::Error + Send + Sync>>;
}
```

Implementations must be **stateless and cheap to clone**. On the server one codec instance lives inside a session and is cloned into every connection task it spawns, so anything expensive or stateful held here is multiplied by the connection count.

The `Send + Sync + 'static` bounds exist because codecs cross task boundaries in the transports.

#### Method `name`

Short identifier used in error messages, e.g. `"json"`. Keep it lowercase and stable; it appears in server logs.

#### Method `encode`

Serializes any `Serialize` value to bytes. Called once per outbound message per recipient.

#### Method `decode`

Deserializes bytes into any `DeserializeOwned` type. Called once per inbound frame. A malformed frame must return `Err` rather than panic: the transports treat a decode failure as a per-message problem and keep the connection open.

### Struct `JsonCodec`

```rust
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonCodec;
```

*Requires the `json` feature, which is on by default.*

Human-readable JSON via `serde_json`. `name()` returns `"json"`. Being a unit struct, it is zero-sized and free to clone.

This is the default codec on every transport in `plaza_session`, chosen because a protocol you can read in a browser console or poke with `websocat` is worth more during development than a compact one. Switch to a binary format when the protocol stabilises.

## 4. Module `payloads`

The netcode vocabulary both ends of a connection exchange. Pure serde, generic over your application types, no math dependency. Re-exported by `plaza` core under `game_common::reconciliation::op_payloads`.

*   **`SequencedClientInput<InputData>`**: client to server. `sequence_number`, `input_data`. The number lets the server report which inputs it has applied, the basis of reconciliation.
*   **`AuthoritativeStateUpdate<PlayerStateData, ServerTimeType>`**: server to client. `last_processed_input_seq`, `authoritative_player_state`, `server_time_at_state`. The client snaps to this and replays newer inputs.
*   **`RemoteEntitySnapshot<EntityKey, ServerTimeType, V3, Q>`**: server to client. `entity_id`, `server_time`, `position`, `rotation`, optional `linear_velocity` / `angular_velocity`. `V3`/`Q` are your position and rotation types (use `()` for a rotation you do not track). No defaults: the wire vocabulary does not mandate a math library.
*   **`TimestampedClientAction<ActionData, ClientTimeType>`**: client to server. `client_action_time`, `action_data`. The timestamp lets the server rewind to when the client acted, for lag compensation.
*   **`Ping` / `Pong`**: either direction. `origin_time_ms`. A `Ping` is echoed back as a `Pong` carrying the same `origin_time_ms`; the sender computes `rtt = now - origin_time_ms`. Pair with `plaza_client_utils::RttEstimator` to smooth the samples.

## 5. Feature Flags

| Feature | Default | Effect |
|---|---|---|
| `json` | yes | Compiles [`JsonCodec`](#struct-jsoncodec) and enables the `serde_json` dependency. With `default-features = false` the crate is the trait and payloads plus `serde` alone. |
