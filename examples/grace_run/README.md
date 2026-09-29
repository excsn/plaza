# grace_run

Four seats delving through locked rooms. It covers the part of session-keeping no other example used: the **held seat** and the duplicate op that a resumed session must not apply twice. `table_manners` clears a seat when its player leaves; this example keeps it. The two halves of `ReconnectTracker` are split across the two examples because telling a kick from a drop needs both.

```sh
./run-native.sh                          # desktop window; hosts and plays (--role host)
./run-native.sh --role client --connect ws://host:8098/ws
./wasm-serve.sh                          # build the browser client, host it on :8098
cargo run -p grace_run --bin scripted    # the whole arc, asserted
```

Grab the coins, take a key, turn it in the door; the party walks through an open door by itself, but **not while a seat is held**. Hirelings fill empty seats after a wait. The panel's buttons cut your own link so you can see what happens when it drops.

## The held seat

A drop calls `ReconnectTracker::on_disconnect` and nothing else: the seat keeps its keys and coins, the party stands at open doors and the tracker is driven from the tick, so the game logic decides an expiry rather than a transport callback. A return inside the window (`on_reconnect` returning true, keyed by presenting the **same** agent id: `/ws?p=<id>`, an auth token's job in a deployment) reclaims everything. The transport never knows a quit from a drop (`lobby_world`'s finding), so every leave gets grace and only the window's expiry is final.

The grace window has a cost whichever way it is set, so each side has a meter. If the hold is too long, the party waits at an open door: `waited_ms` measures that, accruing every tick a held seat keeps an open door shut. If it is too short, a brief wifi drop costs a player their run: `expiries` against `resumes` counts that. The grace slider sets the window. A new value applies only when no hold is running, so a hold already in progress keeps its original window.

## Exactly-once delivery

Every acting op carries its seat's own sequence. The client keeps an **outbox** of everything unacked (the per-seat `acked_seq` in each snapshot is the ack) and after a resume it re-sends the outbox in full: at-least-once, the retry most clients on a flaky link end up writing. The server applies each sequence **at most once**: a sequence at or below the applied mark is a duplicate, suppressed and counted. The two together give exactly-once delivery across a drop.

The dedup has an off switch so you can see the failure it prevents. With it off, the resent `Unlock` finds the door it already opened and the key burns. One door opens and two keys are gone and `keys_burned` counts it. A duplicated op is the one kind of staleness a resync cannot repair.

## Structure

Same listen-server shape as the other playgrounds: one crate builds the authoritative server, the desktop client and the browser client (`--no-default-features --features web`, wrapped by `wasm-build.sh`); MessagePack with a build-derived protocol version. The scripted run covers each case and asserts the meters: a suppressed resend, a held seat resumed with its loot, a key burned with the dedup off and a window that ran out freeing the party.
