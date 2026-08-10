# map_forge

Four editors forge one bomb_grid board together. This is the example for the half of plaza nothing had ever called: every collaborative surface here is `plaza::app_common`'s shipped vocabulary, consumed verbatim rather than mirrored, because the open question was not what a number does but whether the surface works at all.

```sh
./run-native.sh                          # desktop window; hosts and plays (--role host)
./run-native.sh --role client --connect ws://host:8099/ws
./wasm-serve.sh                          # build the browser client, host it on :8099
cargo run -p map_forge --bin scripted    # the whole arc, asserted
```

Lock a region, paint soft and hard walls, drop numbered spawn markers, watch everyone else's cursors live, then hit playtest.

## The four vocabularies, worn

- **`locking`**: the board's quadrants are the resources. `LockManager` answers every paint; a request that loses is a `LockDeniedNoticePayload` with the owner's name in the reason, a leaver's locks are force-released with `by_agent_id: None`, which is exactly the shape that `Option` exists for. Denials are counted.
- **`object_property_ops`**: the board **is** a property object: key `"x,y"`, value a tile name. Painting is `SetObjectPropertyPayload`, erasing is `DeleteObjectPropertyPayload`, and nothing else mutates it.
- **`ordered_collection_ops`**: the spawn roster, where order genuinely means something: roster order is seat order at playtest. Insert, move (by `new_after_item_id` or `new_index`), remove.
- **`presence`**: cursors and coarse activity, relayed rather than stored, at 10Hz. The `CursorPositionPayload` and `ActivityStatusPayload` fragments as shipped.

## Reconciliation without a tick

A game corrects you against a simulation both sides ran; an editor corrects you against a decision only the server made. Paints here are applied **optimistically** the moment they are clicked, confirmed when the snapshot carries them, and **reversed on screen** when the refusal lands, because the region's lock lived on the other machine. The panel counts both sides: refusals the server issued, and reversals this client performed. That pair is this example's version of a netcode panel's corrections.

## The crossing

The playtest is the claim that plaza's two halves compose rather than merely coexist: the property store becomes `bomb_grid::sim::types::Grid`, the roster becomes its seats, and from there the rules are that crate's, running its authoritative `sim::server::Server` inside this controller. Walk with WASD, drop a bomb with SPACE, and the blast that carves your soft walls is bomb_grid's own chain resolution; `walls_carved` counts what the playtests cost the board **live**, while the bench's property store keeps the authored map untouched for when you come back to it.

## Structure

Same listen-server shape as the other playgrounds: one crate builds the authoritative server, the desktop client, and the browser client (`--no-default-features --features web`, wrapped by `wasm-build.sh`); MessagePack with a build-derived protocol version. The scripted run walks all four vocabularies and the crossing, and asserts the meters.
