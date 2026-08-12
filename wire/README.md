# `plaza_wire`

**License:** Mozilla Public License 2.0 (MPL-2.0) · **Status:** Experimental

The wire vocabulary shared by a Plaza server and whatever talks to it: the `WireCodec` trait (with a JSON implementation), and the common netcode payload types both ends exchange.

How to use it: [README.USAGE.md](README.USAGE.md). Full surface: [API_REFERENCE.md](API_REFERENCE.md).

## Install

```toml
[dependencies]
plaza_wire = "0.7"
```

Trait only, no JSON:

```toml
plaza_wire = { version = "0.7", default-features = false }
```

## What it gives you

| Problem | Piece |
|---|---|
| A browser or Dart client naming what it sends, without depending on a server runtime | the crate itself: no async, no tokio, `wasm32` clean |
| Dispatching on a message's kind without parsing its body | `frame::Kind`, one byte ahead of the payload |
| Agreeing where a frame ends on a byte stream | `framing::delimit`, `framing::LengthDelimited` |
| Swapping the wire format without touching transport code | `WireCodec`, with `JsonCodec` / `MsgPackCodec` / `MsgPackNamedCodec` |
| Telling a browser it may `JSON.parse` a frame directly | `WireCodec::is_text` |
| Measuring a round trip, and locating the other end's clock | `frame::Ping` / `Pong`, `frame::answer_ping` |
| The netcode vocabulary both halves share | `payloads` |
| What the turn, round and phase managers wrap into your ops | `flow_payloads` |
| A protocol version that cannot be forgotten during the change that needed it | `build::Wire::detect` |
| Dart types generated from the same definitions the server uses | `build::Wire::dart_types` |
| Packing a hot array past what a derive can reach | `bits`, and `BitCodec` for the one-line version |
| Carrying a packed payload without it being re-encoded byte by byte | `Payload` |

## What lives here

`Agent`, `AgentId`, `Kind` and the framing helpers, the `WireCodec` trait, the netcode payloads, and the flow-control notice payloads (`flow_payloads`: what the turn, round and phase managers wrap into your ops; core re-exports them at their old paths). All of it is genuinely serialized or genuinely shared, which is the rule for this crate: it exists so a **browser client can name what it sends** without depending on core, which pulls tokio and does not target `wasm32-unknown-unknown`.

`MessageTarget`, `PresenceEvent`, `TargetedOp` and `SessionMessage` stay in core. They are server-side routing and plumbing, they are not `Serialize`, and no client ever sees one.

For plain JavaScript clients, the frame layer ships as a single vendorable file: [js/plaza_protocol.js](js/plaza_protocol.js), documented in [js/README.md](js/README.md).

## Features

| Feature | Default | Effect |
|---|---|---|
| `json` | yes | Provides `JsonCodec` and pulls in `serde_json`. Disable to take the trait alone. |
| `build` | no | The build-script half above, including the `Wire` resolver (pulls `syn`, build-time only). Belongs in `[build-dependencies]`, not `[dependencies]`. |
