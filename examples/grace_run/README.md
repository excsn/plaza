# grace_run

Four seats delving through locked rooms, built for the half of session-keeping no example wore: **the held seat**, and the duplicate a resumed session must not spend twice. `table_manners` clears a seat on purpose; this one keeps it on purpose, and the two halves of `ReconnectTracker` deliberately live in different examples because telling a kick from a drop needs both.

```sh
./run-native.sh                          # desktop window; hosts and plays (--role host)
./run-native.sh -- --role client --connect ws://host:8098/ws
./wasm-serve.sh                          # build the browser client, host it on :8098
cargo run -p grace_run --bin scripted    # the whole arc, asserted
```

Grab the coins, take a key, turn it in the door; the party walks through an open door by itself, but **never past a held seat**. Hirelings fill empty seats after a wait. The panel's buttons cut your own link, because the machinery only becomes visible when the link actually drops.

## The held seat

A drop calls `ReconnectTracker::on_disconnect` and nothing else: the seat keeps its keys and coins, the party stands at open doors, and the tracker is driven from the tick so an expiry is a decision the logic makes, not a callback the transport fires. A return inside the window (`on_reconnect` returning true, keyed by presenting the **same** agent id: `/ws?p=<id>`, an auth token's job in a deployment) reclaims everything. The transport never knows a quit from a drop (`lobby_world`'s finding), so every leave gets grace and only the window's expiry is final.

**The window is a bet with a cost on both sides, so it is a dial with two meters.** Hold too long and the party stands at an open door: `waited_ms` prices that, accruing every tick a held seat keeps an open door shut. Hold too short and a hallway's worth of wifi costs somebody their run: `expiries` against `resumes` is that trade, counted. Drag the grace slider and make the bet yourself; the dial lands when no hold is running, so a window in flight keeps its terms.

## Exactly-once, spelled out as two halves

Every acting op carries its seat's own sequence. The client keeps an **outbox** of everything unacked (the per-seat `acked_seq` in each snapshot is the ack), and after a resume it re-sends the outbox in full: at-least-once, the natural retry every client under a flaky link ends up writing. The server applies each sequence **at most once**: a sequence at or below the applied mark is a duplicate, suppressed and counted. Together: exactly-once across a drop.

The dedup has an off switch because the failure it prevents deserves to be seen rather than described: with it off, the resent `Unlock` finds the door it already opened, and the key burns. One door opened, two keys gone, visible in the game rather than in a log; `keys_burned` counts it, and IMPROVEMENTS' line that a duplicated op is the one staleness a resync cannot repair gets its demonstration.

## Structure

Same listen-server shape as the other playgrounds: one crate builds the authoritative server, the desktop client, and the browser client (`--no-default-features --features web`, wrapped by `wasm-build.sh`); MessagePack with a build-derived protocol version. The scripted run walks the entire argument and asserts the meters: a suppressed resend, a held seat resumed with its loot, a key burned with the dedup off, and a window that ran out freeing the party.
