# `plaza_server_utils`

**License:** Mozilla Public License 2.0 (MPL-2.0) · **Status:** Experimental

The server half of real-time netcode, the counterpart to [`plaza_client_utils`](../client_utils/). Where the client crate holds prediction, interpolation, and smoothing, this holds what an authoritative server needs, starting with the rewind that lag compensation is built on.

How to use it: [README.USAGE.md](README.USAGE.md). Full surface: [API_REFERENCE.md](API_REFERENCE.md).

## Install

```toml
[dependencies]
plaza_server_utils = "0.6"
```

Its only dependency is `plaza_client_utils`, for the shared `Interpolatable` and `ToF32` traits, plus `tracing`. No async runtime, so like the client crate it compiles to wasm: a server *simulation* can run in a browser, which the interactive [`netcode_playground`](../examples/netcode_playground/) example relies on.

## What it addresses

| Problem | Piece |
|---|---|
| A client aims at where a target *was* (it renders remotes in the past), so hits must be judged then, not now | `HistoricalStateBuffer` |
| A world has more entities than fit on the wire, and players in different places, so each client needs only what is near it | `relevance` (`SpatialGrid`, `VisibilitySet`, Morton keys) |
| The world has a third axis, and whether it deserves an index is a measurement | `field` (`Field` with `Flat` / `FlatBand` / `Volume`, `Query` instrumentation) |
| A client also cares about entities *no distance query will ever return*: a party across the zone, a followed player, a guild roster | `subscription` (`Subscriptions`, `Audience`) |
| Some of those entities are simulation *inputs*, so dropping the distant ones changes the answer, but sending them all does not scale | `aggregate` (`AggregateTree`) |
| Streaming that set as *entered* and *left* assumes every packet arrives, and one that does not is lost for good | `delta` (`DeltaBaseline`) |
| A bounded number of seats, where a fresh occupant must not inherit the last one's accumulated state | `seats` (`SeatTable`, `Seating`) |
| Seating policy: a lock for games that seat only between rounds, a ranked waitlist, displacement (a bot holds a seat only until a person wants one), seats held across an absence, bot-driven empties | `seats` (`Roster`, composed of `SeatSlots` and `RankedQueue`, both public) |
| A claim about bandwidth should be a number on screen, not an assertion in a README | `meter` (`RateMeter`) |
| A one-shot op with nothing behind it (a `Welcome`, a `Refused`) is lost for good on a lossy link, and nothing in the protocol will ever mention it again | `oneshot` (`Pending`) |
| An accuracy figure taken against the *present* charges a client for a render delay it chose, so the number grows with the buffer depth rather than with anything going wrong | `render_error` (`render_error_at`) |
| ...and that number should be a **rate**, not the session's average, which climbs for ever toward a level it never reaches | `RateMeter::per_sec` (windowed) against `lifetime_per_sec` |

`SetDigest`, `SlotKey`, `SlotAllocator` and `DeltaMirror` are re-exported from [`plaza_client_utils`](../client_utils/) rather than defined here. Both sides of a delta stream have to agree about them exactly, and a browser client needs them and must not inherit a server to get them.

## Relationship to `plaza`

The `plaza` server framework keeps its own reconciliation helpers (`ServerInputBuffer`, `ClientInputTracker`) under `game_common::reconciliation`. Those are coupled to the server runtime; the pieces here are pure and portable, and will grow as more of the server half is decoupled.
