# auction_floor

Items drop, everyone grabs and the server awards each claim. The graphics are minimal because the example is about arbitration.

The example shows three things. The server decides a contested claim once, from the tick each client named and not from when their packet arrived, so ping does not decide who wins. The earliest tick a client may legally name comes from the server's own measurement and not from the client. One event is also split across audiences: the winner and each loser are told something different from the public record.

The `req` field on `Grab` is redundant, which is worth knowing before you copy this. Item ids are monotonic and never reused and a player may hold only one claim per item, so `(player, item)` already identifies which claim a reply concerns. The example was designed around request correlation before that was checked. The final design removed the need. See the declined entry for why an action that names a unique target rarely needs a synthetic id.

## Running it

```sh
./run.sh                                        # http://127.0.0.1:8091, from anywhere
cargo test -p plaza_example_auction_floor       # every claim below, as a test
```

Open two tabs and fight over the same item.

## How a contest is decided

**A claim names a tick. The contest is decided when the item's window closes.**

An item dropped at tick `D` is contestable until `D + 10` (half a second at 20Hz). Every claim for it is collected across that whole window and ranked together at the end. Lowest named tick wins.

Arrival order plays no part and you can check this: drag the **fake extra send delay** slider up to 600 ms and keep playing. Your packets arrive later and later but your results stay the same. A test checks the same property by having the loser ask first.

Ties break on a hash of the player and the item. The hash is arbitrary but fixed. The alternative would be arrival order, which is decided by ping.

## The cheat and what stops it

Naming a low tick is how you win, so the obvious attack is to always name the earliest tick in the window. What stops it is a number the client does not control: a client may not name a tick before its own connection could have seen the drop.

The floor is `dropped_at + (measured_rtt / 2)` in ticks, from `ActixWsPlazaSession::agent_rtt`, which the transport measures with its own WebSocket ping. A player on a 200 ms link cannot claim the first two ticks, but they also did not see the item until then, so the bound costs them nothing. A claim under the floor comes back as `TooEarly` carrying both numbers.

This is the same approach as `lobby_world`'s latency admission: the server bases these bounds on what it measured itself rather than on what the client reports.

## What you are looking at

| On screen | Meaning |
|---|---|
| an item card | value and how many ticks are left in its window |
| **your earliest legal tick** | `drop + n`, derived from your measured RTT. Different for every player |
| **your claims** | one line per `req`, from `waiting` to `won` or `no`, with the reason |
| won / lost / refused | outcomes split three ways, because "not won" hides the difference between losing a contest and sending something invalid |

## The correlation, concretely

The wire has no envelope: a frame is a kind byte and the ops (`plaza_wire::frame`). So `req` lives in the op, which is the pattern an application has to write today:

```rust
Grab { req: u64, item: ItemId, tick: Tick }     // client asks
Awarded { req, item, value, named, margin, contenders }   // to the winner only
Lost    { req, item, to, named, winner_named, contenders } // to each loser only
Refused { req, item, why }                       // to the asker only
Taken   { item, by, value }                      // to everyone and carries no req
```

The split sends one event to different audiences, which `TargetedOp` already supports: `MessageTarget::Agent(winner)` gets one payload, the losers each get another and `MessageTarget::All` gets the public record. Nothing in plaza had to change to do this.

Plaza has nothing that enforces the correlation. Every rejectable op gets a `req` field by convention and nothing checks that the rejection path carries it back. That is an ergonomic gap. This example documents it and does not work around it.

## Verified

`cargo test` covers arbitration, the window bounds, duplicate claims, expiry and the deterministic tie-break. The socket-level flow was also driven against a running server: two bidders contesting one item where the loser asked first, the loser being told both named ticks, the winner's margin, the public record carrying no `req`, three concurrent claims from one client getting three separate correctly-correlated replies, a sub-floor claim refused as `TooEarly` and a second claim on one item refused as `Duplicate`.
