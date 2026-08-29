# turn_gauge

Two order regimes, one battle. A 3v3 whose entire subject is who acts next: one dial switches the same fight between **per-round initiative**, where speed plus a d20 is re-sorted at every round boundary, and a **continuous delay queue** in the FFX style, where the next actor is whoever's gauge fills first and every action pushes its actor back by its cost.

Each side fields a **bruiser** (slow, heavy, can brace behind a shield), a **medic** (mends, and holds two hastes) and a **trickster** (fast, crit-prone, and holds two slows). Every move carries a time weight the delay queue charges for: a jab is quick, a smash mortgages your own next turn. Under initiative a round-slot is a round-slot whatever you do, so **the two regimes price heaviness differently**, and which side of that trade your team's plan sits on is the game. Crits are rolled from the battle seed, so a replay lands the same hits, and battles score into a first-to-three series.

## The claim, and the audit that holds it

The order costs zero bytes in either regime. Speeds, gauges and the battle seed are state both ends already hold, so the upcoming-actors bar at the top of the screen is the client's own derivation ([`src/order.rs`](src/order.rs), compiled into both ends), never a list the server sent. What keeps that honest is the audit: every op feeds an [`OrderMirror`](src/mirror.rs), each turn the server actually opens is compared against the mirror's derivation, and the panel prints the divergence count. It must read zero; the scripted run exits nonzero the moment it does not. The one exception is the snapshot a mid-battle joiner baselines from, which carries the standing round's order as data, because an initiative order was rolled against speeds as they stood at the boundary and a joiner has no way back to them. Derivation takes over at the next boundary.

Under initiative, a haste landing mid-round moves nothing until the boundary re-rolls, and the projected tail of the bar visibly re-sorts when it lands. Under the delay queue the same haste rescales the target's **remaining** wait immediately and the bar re-sorts on the spot. Same fight, same button, two structurally different answers, which is the picture the example exists for.

**Hovering an order is the what-if.** Before you commit, the act list grows a second, dimmed bar: the queue as it would stand if you gave that order, kill removed, slow pushed, smash's time cost charged to yourself. It is computed by the same shared derivation the audit runs on, crit unrolled because a preview promising a crit would be lying half the time, so the planning tool is a second consumer of the example's claim rather than a feature beside it.

## What it says about `flow_control`

The initiative regime is composition over the shipped manager: one `RoundRobinTurnManager` per round, built from the re-rolled order at the boundary, its own turn notices going straight onto the wire as the audit line. No third `TurnManager` implementation was needed, which is the prediction the IDEAS entry carried. The delay regime holds no manager at all: a linear scan and two integer rules, because a gauge has no passes for a manager to walk.

One deviation worth reading before reusing the pattern ([`src/logic.rs`](src/logic.rs), `advance_turn`): the round boundary is decided **before** the last advance rather than read from `Advanced::PassClosed`. Advancing off the round's last actor would wrap the cursor, seat the first actor of the *old* order and emit its notice, and only then report `PassClosed`; under a re-rolling boundary that notice names the wrong unit. `PassClosed` is the right vocabulary for a continuous round-robin, and one turn too late for a pass that re-plans.

## Running it

```sh
./run-native.sh                          # host: a window plus the server; prints the join address
./run-native.sh --role client --connect ws://<host>:8303/ws
./run-native.sh --role headless          # the deployable server
./wasm-serve.sh                          # build the browser client and host it
cargo run -p turn_gauge --bin scripted   # no window, no socket: the audit as a gate
```

A lone commander gets the bot on the other side after a few seconds; a second human takes it if they arrive first. Extra joiners watch. A commander who lets their clock run out is acted for by the server, and a commander who leaves hands their side to the bot, so a battle never stalls on a vacant chair.

## The panel

- **turns audited / diverged**: the mirror's verdict on every turn the server opened. The second number reading zero is the example's claim being enforced, live.
- **series / battle / round / turn**, and how many chairs timed out.
- The act list: the client's projection, current actor first. Exact for the head, speculative past it twice over under the delay queue, since nobody knows the moves to come and it assumes a standard action for each; the speculation shifting when a haste lands is genre truth rather than error.
