# door_policy

The server's right to say no, built from the library's blocks with no transport of its own.

```sh
cargo run -p plaza_example_door_policy
cargo test -p plaza_example_door_policy
```

A tiny arcade: three seats, an account that is also a wallet, and a credit that buys six seconds. Every door rule guards something the game holds, so the rules are load-bearing rather than demonstrated.

This example originally shipped a 322-line hand-written TCP transport, because the rules it needed were impossible on the shipped one: admission could not fail, an agent could not be resolved to a connection, and nothing could end a session. Those became library primitives, and the rewrite deleted the transport. What follows is the recipe.

## The blocks, and what each rule sits on

| rule | keyed on | when it fires | block it uses |
|---|---|---|---|
| per-address cap | the socket | at accept, before anything exists | fallible `AgentFactory`: return `Err(Refusal::saying(frame))` |
| ban | the account | after `Hello` | `close_connection(conn, farewell)` |
| capacity | the account | after `Hello` | same |
| duplicate login | the account | after `Hello` | `connections_of(&key)` + `close_connection` |
| credit expiry | time | when it runs out | `set_deadline(conn, after, farewell)` |
| link floor | the link | never at the door | not a door rule, by anybody: no round trip exists until the connection does |

Identity arrives after admission, and that split is not a design choice: the factory sees a socket, an account arrives later as an op. Only a rule keyed on what a socket shows can refuse for free; everything keyed on an account costs one registration first, and the panel prices it. `Refusal::LinkTooSlow` is defined and deliberately never raised at the door.

## What stayed policy, on purpose

[`src/door.rs`](src/door.rs) is all that remains of the door: the address occupancy, the account claims, the ban list, and which connection loses a duplicate login. Both duplicate policies are implemented and asserted; under `RefuseNewest` the session in progress is untouched, under `KickOldest` the older connection is told `signed in from somewhere else` before its socket closes. The wallet is keyed on the account, so it never splits. None of this could ship as a library default without deciding it for everyone.

Every index the old build kept by hand is gone: `PresenceEvent` carries the `ConnectionId`, `connections_of` resolves an agent, and the farewell is an op of this crate's own vocabulary handed to the library as bytes.

## What the rewrite could not simplify

**Identity is still judged inside `StateLogic`.** A `Hello` arrives as an op, ops have a single consumer, and the controller is it, so ban, capacity and duplicate login run inside the game's rules and the arcade still knows what a ban is. This is the seat-between-the-socket-and-the-game finding, unchanged: the blocks made every *action* possible, and governance still has nowhere to *stand*.

**A farewell's delivery is not observable from the server.** `close_connection` reports that a live connection took the order, not that the reason reached the wire. The tests assert receipt from the client's side instead, which is arguably the honest place to assert it.
