# auction_floor

Items drop, everyone grabs, the server awards each claim. **Minimal graphics, maximal arbitration.**

Three things are being shown. The server decides a contested claim **once**, from what each client *named* rather than when their packet arrived, so ping does not decide who wins. The earliest tick a client may legally name is a bound the server measured rather than one the client asserted. And one event is split across two audiences: the winner and each loser are told something different from the public record.

The `req` field on `Grab` is **redundant** and is worth knowing about before copying this. Item ids are monotonic and never reused, and a player may hold only one claim per item, so `(player, item)` already identifies which claim a reply concerns. The example was designed around request correlation before that was checked, and the mechanics it settled on removed the need. See the declined entry for why an action that names a unique target rarely needs a synthetic id.

## Running it

```sh
./run.sh                                        # http://127.0.0.1:8091, from anywhere
cargo test -p plaza_example_auction_floor       # every claim below, as a test
```

Open two tabs and fight over the same item.

## The one decision everything follows from

**A claim names a tick. The contest is decided when the item's window closes.**

An item dropped at tick `D` is contestable until `D + 10` (half a second at 20Hz). Every claim for it is collected across that whole window and ranked together at the end. Lowest named tick wins.

Nothing about arrival order enters into it, and that is checkable: drag the **fake extra send delay** slider up to 600 ms and keep playing. Your packets arrive later and later; your results do not change. A test pins the same property by having the loser ask *first*.

Ties break on a hash of the player and the item. Arbitrary, but fixed: the alternative is arrival order, and arrival order is ping.

## The cheat, and what stops it

Naming a low tick is how you win, so the obvious attack is to always name the earliest tick in the window. What stops it is a number the client does not control:

> **You may not name a tick before your own connection could have seen the drop.**

The floor is `dropped_at + (measured_rtt / 2)` in ticks, from `ActixWsPlazaSession::agent_rtt`, which the transport measures with its own WebSocket ping. A player on a 200 ms link genuinely cannot claim the first two ticks, and also genuinely did not see the item until then, so the bound costs them nothing they had. A claim under the floor comes back as `TooEarly` carrying both numbers.

This is the same principle as `lobby_world`'s latency admission: the interesting bounds in a server come from what the server measured, never from what the client said.

## What you are looking at

| On screen | Meaning |
|---|---|
| an item card | value, and how many ticks are left in its window |
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
Taken   { item, by, value }                      // to everyone, and carries no req
```

The split is two audiences over one event, which `TargetedOp` already expresses: `MessageTarget::Agent(winner)` gets one payload, the losers each get another, and `MessageTarget::All` gets the public record. Nothing in plaza had to change to do this.

What plaza does *not* have is anything making it structural. Every rejectable op grows a `req` field by convention, and nothing checks that the rejection path carries it back. That is a real ergonomic gap and this example is the evidence for it, not a workaround for it.

## Verified

`cargo test` covers arbitration, the window bounds, duplicate claims, expiry, and the deterministic tie-break. The socket-level flow was also driven against a running server: two bidders contesting one item where the loser asked first, the loser being told both named ticks, the winner's margin, the public record carrying no `req`, three concurrent claims from one client getting three separate correctly-correlated replies, a sub-floor claim refused as `TooEarly`, and a second claim on one item refused as `Duplicate`.
