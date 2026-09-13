# quick_draw

Two duelists wait for one signal and whoever fires first wins. The whole outcome is decided inside one tick. Plaza resolves time to the tick and `InputSchedule` executes an input on the tick the client named, which keeps ping from deciding the winner when the inputs name different ticks. Two inputs naming the **same** tick have no principled tiebreak and fall back to arrival order, which is what `auction_floor` prevents at the next resolution up. At a 20ms tick that leaves a 20ms window in which latency decides the winner and the result looks like skill.

```sh
./run-native.sh                          # desktop window; hosts and plays (--role host)
./run-native.sh --role client --connect ws://host:8096/ws
./wasm-serve.sh                          # build the browser client, host it on :8096
cargo run -p quick_draw --bin scripted   # the in-process arc, mill numbers included
```

One tab and the bot takes the other seat after five seconds. Fire on the signal with a click or SPACE; fire early and you false start.

## The mechanism: a sub-tick offset on the input

A press is sent as `Fire { tick, offset_us }`: the tick it names **and the place inside it**, stamped from the client's estimate of server time (the pump's timeline over pongs and every stamped op; the session's pong clock is the simulation clock). Counter-Strike 2 ships the same approach at 64 ticks. Here it is built on plaza's input model.

The server floors the claim the same way it floors the tick, one resolution finer: it clamps the claim into `[arrival - measured_one_way - slack, arrival]`, so a claim cannot name a moment earlier than your own link could have delivered the press or a moment in the future. The one-way is the session's own probe measurement, the same number `lobby_world` admits players on. The panel prints the limit: a false claim gains at most the slack (30ms here). The floor bounds cheating but does not detect it.

Every contest is resolved **twice**, once by declared stamps and once by arrival order and the verdict carries both. The sub-tick winner scores. The arrival winner is reported alongside it, so each disagreement shows up in play as well as in the totals.

## Measuring the disagreement rate

Genuine simultaneity is rare in a human duel (reaction time is an order of magnitude above a tick), so the panel's rate comes from the **mill**: seeded pairs of presses, thousands a minute, run through the same floor and both resolutions. The mill uses no wall clock and no entropy, so a run replays exactly.

The falsifier is a slider that widens one side's one-way. The **arrival column moves and the declared column must not**, because an honest declared stamp does not depend on where the delay is. A test pins this (`delaying_one_link_moves_arrival_wins_and_not_declared_wins`), including the edge case: matched links **cannot** disagree, since both orders then reduce to press order. They start to disagree only once the links differ. The cheat dial (`A claims early`) drives the floored counter to 100% of contests and shows the bounded gain.

## The deferred extraction

IDEAS filed this with: extract a fractional offset onto `InputSchedule`'s path only if the rate justifies it. The mill's answer at defaults (40ms reaction jitter, one side delayed 130ms): a few percent of contests disagree, all of them where links are uneven. With matched links, zero disagree. So the mechanism makes same-window contests fair *between unequal links* and changes nothing where links match. Whether that justifies the extraction is a product decision. The hand-rolled cost here was ~40 lines (the clamp and the double resolution).

## Structure

Same listen-server shape as the other playgrounds: one crate builds the authoritative server, the desktop client and the browser client; `--no-default-features --features web` is the wasm build (`wasm-build.sh` wraps it); MessagePack with a build-derived protocol version. The bot duels through the same judged path as anyone: an honest claim, an arrival its configured one-way explains, the same clamp.
