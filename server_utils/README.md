# `plaza_server_utils`

**License:** Mozilla Public License 2.0 (MPL-2.0) · **Status:** Experimental

The server half of real-time netcode, the counterpart to [`plaza_client_utils`](../client_utils/). The client crate holds prediction, interpolation and smoothing. This crate holds what an authoritative server needs, starting with the rewind that lag compensation uses.

How to use it: [README.USAGE.md](README.USAGE.md). Full surface: [API_REFERENCE.md](API_REFERENCE.md).

## Install

```toml
[dependencies]
plaza_server_utils = "0"
```

Its only dependency is `plaza_client_utils`, for the shared `Interpolatable` and `ToF32` traits, plus `tracing`. No async runtime, so like the client crate it compiles to wasm: a server *simulation* can run in a browser, which the interactive [`netcode_playground`](../examples/netcode_playground/) example relies on.

## What it addresses

| Problem | Piece |
|---|---|
| A client aims at where a target *was* (it renders remotes in the past), so hits must be judged at that past instant | `HistoricalStateBuffer` |
| A world has more entities than fit on the wire and players in different places, so each client needs only what is near it | `relevance` (`SpatialGrid`, `VisibilitySet`, Morton keys) |
| Building each client's view separately costs the client count; in a dense crowd the same work can be done once per *place* | `relevance` (`SpatialGrid::occupied`, `GridQuantizer::keys_in_radius`: pack each occupied cell once, hand each viewer the cells its view touches) |
| A world with known bounds pays a hash on every cell lookup and a publisher does `viewers x cells-per-view` of them a tick | `relevance` (`CellSpace`: dense indices, `indices_in_radius`, `corner`) |
| Anything keyed by place needs the same addressing: entities by cell, one payload per cell and who is listening to each cell | `relevance` (`CellTable<T>`, one `T` per cell, `clear_each` to rebuild without churning the heap) |
| A bounded arena re-buckets thousands of entities every tick and a hash per cell lookup is the cost | `relevance` (`DenseGrid`: `SpatialGrid`'s surface over a flat `Vec`, swap the type and nothing else) |
| The world has a third axis and whether it deserves an index is a measurement | `field` (`Field` with `Flat` / `FlatBand` / `Volume`, `Query` instrumentation) |
| Bots should hold real seats, off the send path and yield them to people | `seats::Crew` (fill through the roster, prune after displacement) |
| A client also cares about entities *no distance query will ever return*: a party across the zone, a followed player, a guild roster | `subscription` (`Subscriptions`, `Audience`) |
| Some of those entities are simulation *inputs*, so dropping the distant ones changes the answer, but sending them all does not scale | `aggregate` (`AggregateTree`) |
| Streaming that set as *entered* and *left* assumes every packet arrives and one that does not is lost for good | `delta` (`DeltaBaseline`) |
| A bounded number of seats, where a fresh occupant must not inherit the last one's accumulated state | `seats` (`SeatTable`, `Seating`) |
| Seating policy: a lock for games that seat only between rounds, a ranked waitlist, displacement (a bot holds a seat only until a person wants one), seats held across an absence, bot-driven empties | `seats` (`Roster`, composed of `SeatSlots` and `RankedQueue`, both public) |
| Re-describing a world that is not changing costs bandwidth every tick; a change should go out once, to the viewers it concerns | `told` (`Told`, the state half of a change-only stream) |
| Bandwidth should be measured and shown on screen | `meter` (`RateMeter`) |
| A one-shot op with nothing behind it (a `Welcome`, a `Refused`) is lost on a lossy link, since nothing in the protocol sends it again | `oneshot` (`Pending`) |
| An accuracy figure taken against the *present* charges a client for a render delay it chose, so the number grows with the buffer depth even when nothing is wrong | `render_error` (`render_error_at`) |
| ...and that number should be a windowed **rate**, because a session average keeps climbing toward the current level without reaching it | `RateMeter::per_sec` (windowed) against `lifetime_per_sec` |

`SetDigest`, `SlotKey`, `SlotAllocator` and `DeltaMirror` are re-exported from [`plaza_client_utils`](../client_utils/) rather than defined here. Both sides of a delta stream have to agree about them exactly and a browser client needs them and must not inherit a server to get them.

## Relationship to `plaza`

The `plaza` server framework keeps its own reconciliation helpers (`ServerInputBuffer`, `ClientInputTracker`) under `game_common::reconciliation`. Those are coupled to the server runtime; the pieces here are pure and portable and will grow as more of the server half is decoupled.
