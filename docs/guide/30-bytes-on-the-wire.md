# 30. Bytes on the wire

The question this chapter answers: what is actually inside a plaza frame, and how do I ship an update without stranding every client built last week?

## When a codec is not enough

A codec is byte-aligned, and for an envelope that is the right trade. For the hot array in a state-sync packet, where the same field appears once per entity per tick against a budget, it is not: a `bool` costs eight bits to say one thing, and a position costs 96 to express something the game renders at a millimetre.

[`bits`](../../wire/API_REFERENCE.md) is the sub-byte layer for that array: bounded-float quantisation, smallest-three quaternions (29 bits against 128), nibble varints, and the delta-coded indices that make a subset cheap to address. `BitCodec` is the same idea with nothing written by hand, and the gap between them is the honest boundary: **serde's data model has no place to put a bound**, so a derive can shrink a bool and varint an integer but can never know a position is within ±256 at 2mm. Measured on 901 cubes, the derive is worth 1.4x and the hand-written layout 5.0x, and the remaining 3.6x costs a layout *and* a matching reader per type, and is lossy where the derive is lossless.

So the usual shape is to pack one array by hand and leave the envelope on MessagePack. One trap on the way: carry that payload as a `Vec<u8>` and every byte is re-encoded as an integer, costing 15502 bytes to carry 10396. `Payload` is the field type that does not.

## One byte, then your bytes

Every frame is `[kind: u8][codec-encoded body]`. That is the whole format. The kind byte lives *outside* the codec on purpose: inside a serde enum, the codec decides what the tag costs and a reader must parse to dispatch; a byte ahead costs exactly one byte in every format and is read without parsing. The wire crate measured the alternatives (39 bytes and 113ns against 42 bytes and 180ns, with in-document dispatch needing a second parse at 239ns), and the byte won.

There are four kinds: `Ops` (your payload, which is nearly every frame), `Hello` (version handshake), `Ping` and `Pong` (the measurement plane, [chapter 31](31-faking-a-bad-network.md)). The bar for adding a fifth is stated as a test: if application code has to act on it, it is an op, not a kind. Snapshots, kicks, and farewells are all ops for exactly this reason.

Two rules follow from the format and are easy to miss. First, **an unknown kind is skipped, never fatal**, and this rule cannot be added later: a deployed client cannot learn tolerance retroactively, so it is load-bearing from day one. Second, there is no sender identity on the wire; who sent a frame is the server's bookkeeping, attached from the connection it was read on. And plaza is a *stream* wire format: no sequence or fragment fields, so a datagram transport must keep each message inside one datagram, a constraint [chapter 33](33-bring-your-own-socket.md)'s UDP experiment ran into on purpose.

## Codecs: readable first, compact when it pays

The body goes through a `WireCodec`, stateless and swappable. `JsonCodec` is the default because debuggability is a feature: JSON text frames can be read from a browser console or `websocat` with no tooling, and the example browser pages parse the wire with nothing but `JSON.parse`. `MsgPackCodec` is the compact option, about 40% of JSON's size in the general case and measured at 4.2x on horde's real traffic, with a cost worth respecting: positional encoding means field order is the schema, which is precisely what the version handshake below exists to police.

`MsgPackNamedCodec` puts the field names back, and exists for the client that cannot be built from the server's struct definitions: a hand-written or generated model in another language reads names, not positions, and under the compact codec nothing checks that its field order still matches Rust's. Two things about it are worth knowing before you reach for it. **Decode is shared**, because `rmp_serde` dispatches on the MessagePack marker rather than on the type, so one decoder reads a struct arriving as an array *or* as a map and a server reads either shape whichever it writes; a migration can therefore turn one direction at a time instead of flipping both ends together. And **the names are not cheap**: measured over a whole match in [parlour_game](../../examples/parlour_game/), named is 76% of JSON where compact is 26%, a premium of 190%.

That gap points at the rule worth carrying out of this chapter, because the two obvious compaction targets have opposite shapes. **A variant tag is a per-message cost**, so its share is set by your average message size and *small* messages pay most: [curtain_fire](../../examples/curtain_fire/) measures over 15% on a stream of one-line events and about 1% on frame-dominated traffic. **A field name is a per-field cost**, so its share is set by how *wide* a message is and the widest pays most, which is why a per-recipient state view is the expensive case and a two-field notice is not. Establish which of the two you are looking at before you measure anything, because the traffic that is cheap for one is expensive for the other. A hot fieldless enum mapped to a `u8` yourself is the first lever only if your messages are small.

## Shipping an update

The scenario that motivates the handshake: a browser client is a build product, and it does not rebuild when the server does. A page loaded before a wire change still loads, still runs, and only the messages whose shape changed are rejected. That failure reads as a netcode bug and is a deployment bug, and it once cost two rounds of diagnosis. The defense has three parts:

1. **A version nobody has to remember to bump.** Your `build.rs` calls `plaza_wire::build::emit` over your protocol source files; it strips them to type definitions and hashes those, emitting a `WIRE_PROTOCOL: u32`. A version bumped by hand is skipped precisely during the change that needed it. The hash over-asks slightly (a comment change re-versions), which the docs defend as the right direction to be wrong in: the cost is a page load.
2. **Say it once.** The client sends `Hello` carrying its version once on connect, and the server announces its own the same way. Carrying the version per-frame was measured and rejected (53 versus 42 bytes for information that never changes mid-connection). A mismatch is *recorded, not refused*: the number is a build hash, so a recompiled-but-identical peer is indistinguishable from a reshaped one, and what to do (banner, force reload, nothing) is application policy read from `ConnectionManager::protocol`. `UNKNOWN` on either side counts as agreement, so clients from before the handshake keep working, with an honest floor stated in the docs: the handshake cannot rescue a client older than the handshake itself.
3. **Defeat the cache.** None of it helps if the browser serves last week's page from cache; the host module's cache-busting ([chapter 32](32-serving-your-game.md)) is the other half of the same defense.

## More than one language

The wire is defined once, in Rust, and mirrored where clients live: the Dart/Flutter mirror is kept honest by golden fixtures generated from the Rust crate's own tests, each vector emitted three ways (compact msgpack, named msgpack, and JSON as a human reads it), so the mirror is checked against the crate rather than against someone's reading of it. Browser JS needs no mirror at all: kind byte plus JSON text frame is `JSON.parse` territory, which is a deliberate property, not luck.

A page that hand-writes a binary codec loses that property, and one shipped wrong: `parlour_game`'s encoder fell through to its map branch for arrays, so a batch of ops went out as a one-key map, every play was discarded server-side, and the turn timeout played for the player, which reads exactly like a game rule. Nothing compiles a browser page, so [`examples/check_pages.py`](../../examples/check_pages.py) now runs each page's own `mpDecode`/`mpEncode` against the committed fixtures, checks its kind bytes against `plaza_wire::frame::Kind`, and fails a page whose server has a fieldless variant it has no helper for. The page's decoder was exercised constantly and its encoder never, which is the general shape to watch for.

## Ripping it apart

The codec is the seam: implement `WireCodec` (encode, decode, `is_text`) and every transport and session works with your format, bincode, postcard, whatever, with the kind byte untouched ahead of it. The frame layout itself is the one thing this guide will tell you not to rip apart, because both ends of every client you will ever ship have to agree on it, and the skip-unknown-kinds rule is the escape hatch that lets the format grow instead.

## The lab

Take any browser example, open the network tab, and read the frames raw; that is the JSON argument making itself. Then switch a playground's codec to `MsgPackCodec`, put [chapter 11](11-keeping-the-pipe-small.md)'s meter on screen, and watch what compactness actually buys on your traffic rather than in a benchmark.
