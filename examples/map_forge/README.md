# map_forge

Four editors build one bomb_grid board together. No example had called `plaza::app_common` before this one. Every collaborative surface here uses its shipped vocabularies directly instead of copying them, because the open question was whether that API works at all.

```sh
./run-native.sh                          # desktop window; hosts and plays (--role host)
./run-native.sh --role client --connect ws://host:8099/ws
./wasm-serve.sh                          # build the browser client, host it on :8099
cargo run -p map_forge --bin scripted    # the whole arc, asserted
```

Lock a region, paint soft and hard walls, drop numbered spawn markers, watch everyone else's cursors live, then hit playtest.

## The four vocabularies

- **`locking`**: the board's quadrants are the resources. `LockManager` answers every paint. A request that loses gets a `LockDeniedNoticePayload` with the owner's name in the reason. A leaver's locks are force-released with `by_agent_id: None`, which is the case that `Option` is for. Denials are counted.
- **`object_property_ops`**: the board **is** a property object: key `"x,y"`, value a tile name. Painting is `SetObjectPropertyPayload`, erasing is `DeleteObjectPropertyPayload` and nothing else mutates it.
- **`ordered_collection_ops`**: the spawn roster, where order matters: roster order is seat order at playtest. Insert, move (by `new_after_item_id` or `new_index`), remove.
- **`presence`**: cursors and coarse activity at 10Hz, which the server relays without storing. The `CursorPositionPayload` and `ActivityStatusPayload` fragments as shipped.

## Reconciliation without a tick

An editor has no shared simulation to correct you against; its corrections come from decisions only the server made. Paints here are applied **optimistically** the moment they are clicked, confirmed when the snapshot carries them and **reversed on screen** when the refusal lands, because the region's lock lived on the other machine. The panel counts both sides: refusals the server issued and reversals this client performed. That pair takes the place of the corrections count on a netcode panel.

## Playtest

The playtest hands the edited board to plaza's game side: the property store becomes `bomb_grid::sim::types::Grid`, the roster becomes its seats and from there bomb_grid's rules apply, with its authoritative `sim::server::Server` running inside this controller. Walk with WASD and drop a bomb with SPACE. The blast that carves your soft walls is bomb_grid's own chain resolution. `walls_carved` counts the walls the playtests destroy on the **live** board, while the bench's property store keeps the authored map unchanged for when you come back to it.

## Structure

Same listen-server shape as the other playgrounds: one crate builds the authoritative server, the desktop client and the browser client (`--no-default-features --features web`, wrapped by `wasm-build.sh`); MessagePack with a build-derived protocol version. The scripted run walks all four vocabularies and the playtest and asserts the meters.
