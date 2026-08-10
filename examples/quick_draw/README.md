# quick_draw

Two duelists, one signal, and whoever fired first wins: the smallest game whose entire outcome lives inside one tick. The tick is plaza's resolution of truth, and `InputSchedule` executes an input on the tick the client named, which is the whole answer to "ping must not decide the winner"; but two inputs naming the **same** tick have no principled tiebreak and fall back to arrival order, which is precisely what `auction_floor` exists to prevent, one resolution coarser. At a 20ms tick that is a 20ms window in which the game decides by latency and reports it as a draw of skill.

```sh
./run-native.sh                          # desktop window; hosts and plays (--role host)
./run-native.sh --role client --connect ws://host:8096/ws
./wasm-serve.sh                          # build the browser client, host it on :8096
cargo run -p quick_draw --bin scripted   # the in-process arc, mill numbers included
```

One tab and the bot takes the other seat after five seconds. Fire on the signal with a click or SPACE; fire early and you false start.

## The mechanism: an offset on the tick-addressed input, floored like the tick

A press is sent as `Fire { tick, offset_us }`: the tick it names **and the place inside it**, stamped from the client's estimate of server time (the pump's timeline over pongs and every stamped op; the session's pong clock is the simulation clock). That is Counter-Strike 2's shipped answer at 64 ticks, worn by plaza's input model.

The claim gets the same treatment the tick gets, one resolution finer: the server clamps it into `[arrival - measured_one_way - slack, arrival]`. You cannot name a moment your own link says your press could not have reached, and you cannot name the future. The one-way is the session's own probe measurement, the same number `lobby_world` admits players on. The honest statement, printed on the panel: **a dishonest claim gains at most the slack** (30ms here); the floor bounds cheating, it does not detect it.

Every contest is resolved **twice**, once by declared stamps and once by arrival order, and the verdict carries both. The sub-tick winner is the one that scores; the arrival winner rides along so every disagreement is visible in play, not only in aggregate.

## The number, and the falsifier

Genuine simultaneity is rare in a human duel (reaction time is an order of magnitude above a tick), so the panel's rate comes from the **mill**: seeded pairs of presses, thousands a minute, run through the same floor and both resolutions. Deterministic by construction, no wall clock and no entropy, so a run replays exactly.

The falsifier is a slider: widen one side's one-way and the **arrival column moves while the declared column must not**, because an honest declared stamp does not care where the delay lives. That is pinned as a test (`delaying_one_link_moves_arrival_wins_and_not_declared_wins`), with its sharp edge: matched links **cannot** disagree, since both orders then reduce to press order; the daylight opens exactly as the links skew. The cheat dial (`A claims early`) drives the floored counter to 100% of contests and shows the bounded gain.

## What this says about the deferred extraction

IDEAS filed this with: extract a fractional offset onto `InputSchedule`'s path only if the rate justifies it. The mill's answer at defaults (40ms reaction jitter, one side delayed 130ms): a few percent of contests disagree, concentrated entirely where links are uneven; with matched links, zero. So the mechanism buys fairness *between unequal links* in same-window contests, and buys nothing where links match. Whether that justifies the extraction is a product judgment the numbers now inform; the hand-rolled cost here was ~40 lines (the clamp and the double resolution).

## Structure

Same listen-server shape as the other playgrounds: one crate builds the authoritative server, the desktop client, and the browser client; `--no-default-features --features web` is the wasm build (`wasm-build.sh` wraps it); MessagePack with a build-derived protocol version. The bot duels through the same judged path as anyone: an honest claim, an arrival its configured one-way explains, the same clamp.
