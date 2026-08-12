# 40. The right to say no

The question this chapter answers: how do I refuse, bound, and end a connection, kick, ban, time out, expire, drain, without the library deciding any of those policies for me?

If you are building an app rather than a game, note that this chapter is *more* yours, not less: moderation, rate limiting, session expiry, and graceful deploys are the daily business of collaborative tools, and everything below is written for both.

## The division that organizes everything

Governance in plaza follows one rule with no exceptions: **mechanism in the layer that sees the traffic, policy above it.** The session can refuse a socket, resolve an agent to its connections, close a connection with a farewell, read per-connection activity and volume, and enforce a deadline. The session never knows *why*: ban lists, thresholds, timeouts, and which duplicate login wins are yours, and where a default would decide policy for everyone, none ships. There is no `kick_after` option anywhere in plaza, and that absence is a feature with a design behind it.

A matched principle keeps the game side simple: **a forced disconnect looks exactly like a cable pull.** Every departure arrives as the same `Left` event, so your logic keeps one disconnect story, and what a removal *means* (seat cleared versus seat held, [chapter 12](12-players-come-and-go.md)) is expressed in your state, not in a second presence vocabulary.

## The mechanisms, in the order a connection meets them

**Admission.** The door can say no before anything exists: the TCP `AgentFactory` returns a result, and a refusal registers nothing, announces nothing, and optionally writes one farewell frame on the way out; on WebSockets your HTTP route is the door. But identity is not available at the door, and that is not a limitation to engineer around, it is the structure of the problem: the door sees a socket, an account arrives later as an op. So rules split by when they can be known. A per-address cap can refuse for free; a ban, a capacity rule, or a duplicate login must admit first and undo after, at the cost of one registration. The door_policy lab prices that difference in a printed table.

**Resolution.** A decoded op names an agent; acting needs a connection. `connections_of` bridges the two, and presence events carry the connection id, so "this account must go" always ends with a handle you can act on, even when the account holds two sockets.

**The close.** `close_connection` flushes what was queued, writes your farewell *last*, then closes the socket. The farewell is an op of your own vocabulary, pre-encoded and handed to the session as bytes; "removed by the host" versus "away from the table" is application language, and no transport close code could carry it. `deregister_agent` closes every connection an account holds; `disconnect_all` drains the room through the same path, told then closed, which makes a graceful shutdown the same mechanism as a kick applied to everyone.

**The readers.** `idle_for` answers how long a connection has been silent, and only the session can answer it honestly, because the session answers latency probes invisibly; an AFK rule written above the session either counts probe traffic as presence or never fires. `connection_inbound` counts frames and bytes per connection, which the session-wide stats cannot attribute; your threshold plus that counter is a flood rule. Both are readers, not timers: you sweep from your own loop and apply your own numbers.

**The deadline.** `set_deadline` bounds a session, enforced by the connection's own task; setting it again is renewal. An arcade credit, a trial period, and an auth token expiry are all this one mechanism wearing different policies.

## Policy worked examples

The two labs were built as governance explorations first and pressure-tested the library into its current shape (their hand-written transports were deleted once the primitives existed, per [chapter 33](33-bring-your-own-socket.md)'s loop):

- [door_policy](../../examples/door_policy/) is admission and identity: a per-IP cap at the factory, a ban and a capacity rule after the Hello, a credit that buys minutes, and both duplicate-login policies (refuse the newcomer, kick the older) implemented in twenty lines each, because the mechanism is resolution plus a close and the choice of loser is nobody's but yours.
- [table_manners](../../examples/table_manners/) is moderation at a table: kicks that say why, AFK removal that probe traffic cannot postpone, a flooder removed without a bystander losing a frame, a drain that tells everyone before closing them, and the kick-versus-drop seat fate from [chapter 12](12-players-come-and-go.md), with every claim asserted as a count.

Beyond removal, authority has subtler shapes the examples also carry: [auction_floor](../../examples/auction_floor/) arbitrates contested claims with a floor built from what the server measured rather than what the client said, and [ghost_trials](../../examples/ghost_trials/) verdicts a lap time by replaying the claimant's own inputs, anti-cheat as a single equality. Governance is not only ending sessions; it is every place the server's word outranks a client's.

[gow_3d](../../examples/gow_3d/) is the case where the server's word is deliberately *weaker* than a client's, and it is here rather than in a netcode chapter because giving that ground away is a governance decision. The client owns its own position and the server only sanity-checks; what it buys is perfectly smooth local movement with nothing to reconcile, and what it costs is a class of cheating that cannot be closed, only bounded. The honest part is the shape of the bound, which is a cliff and not a curve: a 1.3x speed cheat works at exactly 1.3x, and a 2.0x one achieves 0.13x of an honest run. Nothing separates the first from a late packet, because they are the same observation, and a threshold tight enough to catch it throws out honest players on bad connections.

Two things about writing that validator generalise well past games, and both were mistakes before they were lessons.

**A rate check credited per request is a rate multiplier.** Measuring each claim against the time since the last one falsely refuses requests that bunch up, since two arriving in the same millisecond measure zero elapsed against each other. The obvious repair, credit a minimum interval, opened a hole twice as bad as the one it closed: whatever you credit per request is a rate the caller can claim at will, and a client sending twice per tick got a full tick each time, so a 2.0x cheat passed at a full 2.00x. The shape that holds is an **accruing budget** spent by the action, because it accrues from the clock however many times it is asked. That is the same conclusion rate limits, token refills and quota accounting reach, and for the same reason.

**A refusal does not stop anyone, it makes them loud.** The budget must be capped or a disconnection becomes a teleport, and even capped, a client that hammers an impossible claim eventually banks enough to land one. Measured over twenty seconds of doing exactly that: **180 units gained against the 187 an honest runner covers, and 591 refusals logged.** The cheat ends up behind and leaves a trail the whole way. Counting refusals is therefore not an afterthought, it is the entire defence, and a number that reads zero for every honest client is a number worth putting on a panel.

## Ripping it apart

There is nothing to rip: this surface *is* blocks, extracted precisely so the assembled policies could stay in application code. The two example READMEs are the recipes, and what they could not simplify is recorded there too, honestly: the one seam still missing is a place for governance to observe traffic without living inside the game's rules, and the direction doc keeps that deliberately unbuilt until an example forces its shape.

## The lab

Run [door_policy](../../examples/door_policy/) and read the panel it prints: what each refusal cost, judged at the door versus after identity. Then [table_manners](../../examples/table_manners/)' test suite, where "the reason arrived before the socket closed" and "nobody else paid for the flood" are assertions, not aspirations.
