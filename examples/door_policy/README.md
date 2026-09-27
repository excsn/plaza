# door_policy

The server's right to say no, built from the library's blocks with no transport of its own.

```sh
cargo run -p plaza_example_door_policy
cargo test -p plaza_example_door_policy
```

A tiny arcade: three seats, an account that is also a wallet and a credit that buys six seconds. Every door rule guards something the game holds, so the rules are load-bearing rather than demonstrated.

This example originally shipped a 322-line hand-written TCP transport, because the rules it needed were impossible on the shipped one: admission could not fail, an agent could not be resolved to a connection and nothing could end a session. Those became library primitives and the rewrite deleted the transport. One finding survived that rewrite: identity arrived as an op, ops have one consumer and the controller is it, so ban, capacity and duplicate login still ran inside the game's rules and every refusal keyed on an account cost one registration first. The credential frame and the connection admitter closed that gap. What follows is the recipe as it stands now.

## The blocks and what each rule sits on

| rule | keyed on | when it fires | block it uses |
|---|---|---|---|
| unreadable credential | the credential | in the admitter, before anything exists | `ConnectionAdmission::Refused(Farewell)` |
| per-address cap | the socket | in the admitter, on `Peer.addr` | same |
| ban | the account | in the admitter | same |
| capacity | the account | in the admitter, against the door's own book | same |
| duplicate login | the account | in the admitter | `RefuseNewest`: refuse; `KickOldest`: `deregister_agent(old, Farewell)` then admit |
| credit expiry | time | when it runs out | `set_deadline(conn, after, Farewell)` |
| a socket that presents nothing | the timer | `Limits::credential_timeout` after the open | the session's own `Goodbye` 4408 |
| link floor | the link | never at the door | not a door rule, by anybody: no round trip exists until the connection does |

The account is the credential. A client sends it as a `Kind::Credential` right after its `Hello` and the `Doorman` in [`src/door.rs`](src/door.rs) judges every rule on it before the connection registers, so a refused socket is never announced, never seated and never snapshotted. The panel's headline number, connections registered before an identity rule could be judged, is now zero by construction; the ledger's `registered` counts the same thing as the transport's `admitted`.

Each refusal travels as a close code in the application range with the reason's text as the goodbye's detail and the client reads it back through `Refusal::from_code`. A kicked session hears 4409 with `signed in from somewhere else`, a spent credit 4402 with `your credit ran out`. `Refusal::LinkTooSlow` is defined and deliberately never raised at the door.

## What stayed policy, on purpose

[`src/door.rs`](src/door.rs) is all that remains of the door: the address occupancy, the account claims, the ban list, which connection loses a duplicate login and the admitter that applies them. Both duplicate policies are implemented and asserted; under `RefuseNewest` the session in progress is untouched, under `KickOldest` the admitter ends the older connection itself before it admits the newcomer, which works because the loser is registered and the newcomer is not yet. The wallet is keyed on the account, so it never splits. None of this could ship as a library default without deciding it for everyone.

Every index the old build kept by hand is gone: `PresenceEvent` carries the `ConnectionId`, `connections_of` resolves an agent and the reason for a close is a code and a detail the library carries without knowing the vocabulary.

## What the admitter cannot see

**Capacity is judged against the door's own book, not the authoritative state.** The admitter runs before the controller hears of the connection, so it cannot read `ArcadeState.players`; it counts the accounts it has admitted and not yet seen leave, which is the same number one presence event later. A rule that must read game state at admission time has nowhere to run and belongs behind an `OpGuard` or a shared counter instead.

**A goodbye's delivery is not observable from the server.** `deregister_agent` and `set_deadline` report that a live connection took the order, not that the goodbye reached the wire. The tests assert receipt from the client's side instead, which is arguably the honest place to assert it.

**Every client here shares one address**, so the per-address rule is shown with a cap of two through `arcade_with`, below the seat count.
