# held_fire

An action interrupted by the enemy's window. A 3v3 grid skirmish in the listen-server shape (native window, wasm browser page, headless server, bots on empty sides) where a unit on **overwatch** may interrupt an enemy march that crosses its lane, mid-path: the march suspends, an offer goes to the defending commander alone, and FIRE or HOLD decides whether the mover walks on.

## The suspended action

A march is the one op in this workspace that does not resolve inside the `process_input` that accepted it. The server computes the canonical path and walks it **one cell per step window** ([`STEP_MS`](src/protocol.rs)), holding the march as pending state between windows; entering a watcher's lane opens the offer, and every other order is refused while the march owns the floor. Suspend, scoped offer, resume: the verbs the IDEAS entry sent this example to find.

## The window hides in the cadence

The overwatch decision gets exactly one step window, because the answer, whatever it is and whenever it arrives inside the window, is only **applied when the window closes**. Fire, hold and silence therefore all cost the mover the same wall-clock, which is the example's central claim: **a held shot is invisible on the mover's wire**. A test asserts it as an equality of op streams, byte for byte, between a march past a hidden watcher who held every offer and the same march with no watcher at all. Silence holds, so a lapsed clock never shoots on the defender's behalf.

The claim reaches further than the events. A live "offers 3, held 3" on the mover's panel would leak the held shots through arithmetic, so commanders' live views mask the offer counters and the full ledger opens when the battle ends; spectators see it throughout.

## Fog, and the ambush band

Each side sees an enemy only while one of its living units has walking sight of it ([`SIGHT`](src/protocol.rs) = 5, Bresenham, rocks block); an unseen enemy is **absent from the payload**, not flagged in it, and step events are audience-filtered the same way. A watcher's lane reaches further ([`WATCH_REACH`](src/protocol.rs) = 7), and that asymmetry is load-bearing: with symmetric sight an ambush is geometrically impossible, since whoever sees you is seen. The band between 5 and 7 is where overwatch fire arrives from nowhere, the panel counts it as an ambush, and firing reveals the shooter to everyone until the round ends.

## Playing it

```sh
./run-native.sh                          # host: window plus server; prints the join address
./run-native.sh --role client --connect ws://<host>:8304/ws
./run-native.sh --role headless
./wasm-serve.sh                          # browser client on the same port
cargo run -p held_fire --bin scripted    # no window: the offers and the ledger as a gate
```

On your activation (banner and clock at the bottom), click one of your fresh units, then a blue cell to march, a gold-ringed enemy to shoot, or **OVERWATCH**. When your watcher's lane is crossed, the offer fills the screen: FIRE or HOLD under a draining bar, and doing nothing holds. A lone commander gets the bot; extra joiners see the whole board.

## The panel

Battles, rounds and activations; offers against fired, held and lapsed (masked while you fight, for the reason above); marches cut short; ambushes; chairs timed out. The numbers the IDEAS entry asked for: what the decision window costs, and how often it is used at all.
