# door_policy

The server's right to say no, and what plaza currently charges for it.

```sh
cargo run -p plaza_example_door_policy
cargo test -p plaza_example_door_policy
```

A tiny arcade: three seats, an account that is also a wallet, and a credit that buys six seconds. Every door rule guards something the game holds, so the rules are load-bearing rather than demonstrated.

## What this found

Plaza has no session governance at all, and the gaps are larger than "missing helpers". Three of them make an application unable to express the rule rather than awkward at it.

**Admission cannot fail.** `AgentFactory<ID> = Fn(SocketAddr) -> Agent<ID>` returns an agent, not a result, and the TCP accept loop registers whatever it returns. A per-address cap is decidable from exactly what that factory already sees, and there is still no way to refuse. This example puts the same decision before `register`, and the panel prices the difference: the address rule costs **nothing**, while a ban costs **one registration** before it can be applied at all.

**There is no server-initiated close.** `deregister` removes a connection from the registry, and the shipped TCP task has no arm watching for that: its outbound receiver goes quiet while the socket stays open and the client keeps being read and forwarded. An application cannot end a session. The loop here owns its socket, so it writes the reason and *then* shuts the write half, and the panel asserts what that buys: **0 ops accepted after a close**, and every reason delivered.

**An agent cannot be resolved to a connection.** `PresenceEvent` carries an `Agent` and no `ConnectionId`, and there is no `connections_of`, so an application can learn that someone must go and still hold no handle to send them anywhere. Every index in [`src/door.rs`](src/door.rs) exists because of this one.

**Unplanned, and the one that changed the shape of the example**: `subscribe_to_incoming_messages` has a single consumer, and the controller takes it. There is no second place to watch inbound ops from, so admission had to be judged **inside `StateLogic`**. That is the wrong home: the arcade's rules now know what a ban is. Governance wants to sit between the socket and the game, and today there is no such seat.

**And one the entry predicted wrongly.** It listed a link-quality floor as a door rule, moved from `lobby_world`. It cannot be one, by anybody: there is no round trip until the connection exists, so a link rule is inherently accept-then-measure. `Refusal::LinkTooSlow` is defined and deliberately never raised at the door.

## Identity arrives after admission, which is the root of most of it

The door sees a socket. An account arrives later, as an op. So the rules split in two, and the split is not a design choice:

| rule | keyed on | when it can fire | cost of refusing |
|---|---|---|---|
| per-address cap | the socket | at accept | nothing |
| ban | the account | after `Hello` | a registration |
| capacity | the account | after `Hello` | a registration |
| duplicate login | the account | after `Hello` | a registration |
| link floor | the link | never at the door | not a door rule |

## Duplicate login is mechanism plus policy

Both policies are implemented and both are asserted: under `RefuseNewest` the session in progress is untouched and the newcomer is told `already inside`; under `KickOldest` the older connection is told `signed in from somewhere else` and the newcomer is admitted. **The losing connection learns it lost in both cases**, which is the part that needs flush-then-close, and the wallet is keyed on the account so it never splits.

## Extraction this earns

Only what the example had to write by hand:

- a **fallible admission hook**, `Fn(SocketAddr) -> Result<Agent<ID>, Refusal>`, called before `register`
- **`connections_of(&ID)`** and **`deregister_agent(&ID)`**, plus a `ConnectionId` on `PresenceEvent`
- a **flush-then-close disconnect**: write queued frames, then shut the socket, so a farewell survives the close it precedes
- a **per-connection deadline**, swept by the session rather than by an application timer
- a way to **observe inbound ops without taking the controller's stream**, since governance cannot live in the game's rules

A duplicate-login policy on `SessionOptions` is *not* on this list. The mechanism is `connections_of` plus a close; which connection loses is the application's, and putting it in the library would decide it for everyone.
