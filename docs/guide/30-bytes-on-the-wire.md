# 30. Bytes on the wire

This chapter covers what is inside a plaza frame and how to ship an update without stranding every client built last week.

## When a codec is not enough

A codec is byte-aligned, which suits an envelope. It does not suit the hot array in a state-sync packet, where the same field appears once per entity per tick against a budget: a `bool` costs eight bits to carry one and a position costs 96 bits for something the game renders at a millimetre.

[`bits`](../../wire/API_REFERENCE.md) is the sub-byte layer for that array: bounded-float quantisation, smallest-three quaternions (29 bits against 128), nibble varints and the delta-coded indices that make a subset cheap to address. `BitCodec` does the same with nothing written by hand. The gap between them has one cause: **serde's data model has no place to put a bound**, so a derive can shrink a bool and varint an integer but can never know a position is within ±256 at 2mm. Measured on 901 cubes, the derive saves 1.4x and the hand-written layout 5.0x. The remaining 3.6x costs a layout *and* a matching reader per type and is lossy where the derive is lossless.

The usual approach is to pack one array by hand and leave the envelope on MessagePack. If you carry that payload as a `Vec<u8>`, every byte is re-encoded as an integer, costing 15502 bytes to carry 10396. The `Payload` field type avoids that.

## The frame format

Every frame is `[kind: u8][codec-encoded body]` and nothing else. The kind byte sits *outside* the codec on purpose: inside a serde enum, the codec decides what the tag costs and a reader must parse to dispatch, while a leading byte costs exactly one byte in every format and is read without parsing. The wire crate measured the alternatives (39 bytes and 113ns against 42 bytes and 180ns, with in-document dispatch needing a second parse at 239ns) and chose the byte.

There are four kinds: `Ops` (your payload, which is nearly every frame), `Hello` (version handshake), `Ping` and `Pong` (the measurement plane, [chapter 31](31-faking-a-bad-network.md)). A new message that application code has to act on is an op rather than a new kind, which is why snapshots, kicks and farewells are all ops.

Two rules follow from the format. First, **an unknown kind is skipped rather than treated as fatal**. This had to be there from the first release, because a deployed client cannot be taught to tolerate new kinds after the fact. Second, there is no sender identity on the wire; the server attaches the sender from the connection the frame was read on. Plaza is also a *stream* wire format with no sequence or fragment fields, so a datagram transport must keep each message inside one datagram. [Chapter 33](33-bring-your-own-socket.md)'s UDP experiment runs into that constraint on purpose.

## Codecs

The body goes through a `WireCodec`, stateless and swappable. `JsonCodec` is the default because it is easy to debug: JSON text frames can be read from a browser console or `websocat` with no tooling and the example browser pages parse the wire with nothing but `JSON.parse`. `MsgPackCodec` is the compact option, about 40% of JSON's size in the general case and measured at 4.2x smaller on horde's real traffic. Its cost is that positional encoding makes field order the schema, which is what the version handshake below checks.

`MsgPackNamedCodec` puts the field names back. It is for clients that cannot be built from the server's struct definitions: a hand-written or generated model in another language reads fields by name rather than position and under the compact codec nothing checks that its field order still matches Rust's. Two things to know about it. **Decode is shared**, because `rmp_serde` dispatches on the MessagePack marker rather than on the type, so one decoder reads a struct arriving as an array *or* as a map and a server reads either shape whichever it writes. A migration can therefore switch one direction at a time instead of flipping both ends together. **The names are expensive**: measured over a whole match in [parlour_game](../../examples/parlour_game/), named is 76% of JSON where compact is 26%, a premium of 190%.

The two obvious things to compact behave differently. **A variant tag is a per-message cost**, so its share depends on your average message size and *small* messages pay most: [curtain_fire](../../examples/curtain_fire/) measures over 15% on a stream of one-line events and about 1% on frame-dominated traffic. **A field name is a per-field cost**, so its share depends on how *wide* a message is and the widest pay most, which is why a per-recipient state view is the expensive case and a two-field notice is not. Work out which of the two you are looking at before you measure anything, because traffic that is cheap for one is expensive for the other. Mapping a hot fieldless enum to a `u8` yourself is the first thing to try only if your messages are small.

## What an event must carry

Most fields on a wire describe state, so a lost frame costs freshness and the next one repairs it. An event happens once and no later frame mentions it, so you have to decide what it carries up front and cannot add to it later.

The problem is easy to miss because the event works for whatever uses it at the time. [gow_3d](../../examples/gow_3d/) sent a landed ability as a list of seats, which was all a coloured flash needed. It looked right while the flash was the only consumer. Adding an animation needed two more facts from the same event (which ability and what it reached) and **neither can be recovered afterwards**: no frame repeats the landing and the victim's health has already changed by the time the next one arrives, so the amount and the target would have to be inferred from a world that has already changed.

Carry what the event's consumers need at the instant it happens and be suspicious when the current consumer is trivial, because a trivial consumer tells you nothing about what the field is worth. In return, the state does not have to carry these fields: gow_3d's landing says who cast, what and on whom and none of that appears anywhere else on its wire.

Send events that hit nothing too. A swing through empty air is sent, because otherwise a miss looks the same as a key that did nothing, which is a different bug.

## Shipping an update

A browser client is a build product and does not rebuild when the server does. A page loaded before a wire change still loads and runs and only the messages whose shape changed are rejected. That looks like a netcode bug but is a deployment bug and it once cost two rounds of diagnosis. Plaza handles it in three parts:

1. **An automatic version.** Your `build.rs` calls `plaza_wire::build::emit` over your protocol source files; it strips them to type definitions and hashes those, emitting a `WIRE_PROTOCOL: u32`. A version bumped by hand tends to be forgotten during exactly the change that needed it. The hash is slightly too sensitive (a comment change re-versions), which the docs accept because a false mismatch only costs a page load.
2. **Sent once per connection.** The client sends `Hello` carrying its version once on connect and the server announces its own the same way. Carrying the version per-frame was measured and rejected (53 versus 42 bytes for information that never changes mid-connection). A mismatch is recorded rather than refused: the number is a build hash, so a recompiled but identical peer looks the same as a reshaped one. What to do about it (banner, force reload, nothing) is application policy read from `ConnectionManager::protocol`. `UNKNOWN` on either side counts as agreement, so clients from before the handshake keep working. The docs state the limit: the handshake cannot help a client older than the handshake itself.
3. **Cache busting.** None of this helps if the browser serves last week's page from cache, so the host module's cache-busting ([chapter 32](32-serving-your-game.md)) covers that part.

## More than one language

The wire is defined once in Rust and mirrored in the other languages clients use. The Dart/Flutter mirror is checked against golden fixtures generated from the Rust crate's own tests, each vector emitted three ways (compact msgpack, named msgpack and JSON as a human reads it), so the mirror is tested against the crate itself rather than against someone's reading of it. Browser JS needs no mirror at all: a kind byte plus a JSON text frame can be read with `JSON.parse`, by design.

A page that hand-writes a binary codec loses that and one shipped with a bug: `parlour_game`'s encoder fell through to its map branch for arrays, so a batch of ops went out as a one-key map, every play was discarded server-side and the turn timeout played for the player, which looked exactly like a game rule. Nothing compiles a browser page, so [`examples/check_pages.py`](../../examples/check_pages.py) now runs each page's own `mpDecode`/`mpEncode` against the committed fixtures, checks its kind bytes against `plaza_wire::frame::Kind` and fails a page whose server has a fieldless variant it has no helper for. The page's decoder was exercised constantly and its encoder never. Watch for code where one direction is tested and the other never runs.

## Replacing it

The codec is the seam: implement `WireCodec` (encode, decode, `is_text`) and every transport and session works with your format (bincode, postcard or anything else), with the kind byte still in front of it. Do not replace the frame layout itself, because both ends of every client you ship have to agree on it. The skip-unknown-kinds rule is how the format grows instead.

## The lab

Take any browser example, open the network tab and read the raw frames. Then switch a playground's codec to `MsgPackCodec`, put [chapter 11](11-keeping-the-pipe-small.md)'s meter on screen and see how much compactness saves on your own traffic rather than in a benchmark.
