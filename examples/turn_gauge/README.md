# turn_gauge

A 3v3 battle about who acts next, with two turn-order regimes. One dial switches the same fight between **per-round initiative**, where speed plus a d20 is re-sorted at every round boundary and a **continuous delay queue** in the FFX style, where the next actor is whoever's gauge fills first and every action pushes its actor back by its cost.

Each side fields a **bruiser** (slow, heavy, can brace behind a shield), a **medic** (mends and holds two hastes) and a **trickster** (fast, crit-prone and holds two slows). Every move carries a time weight the delay queue charges for: a jab is quick, a smash delays your own next turn. Under initiative every move takes one round-slot whatever it is, so the two regimes charge differently for heavy moves and the best plan depends on the regime. Crits are rolled from the battle seed, so a replay lands the same hits and battles score into a first-to-three series.

## The derived order and its audit

The order costs zero bytes in either regime. Speeds, gauges and the battle seed are state both ends already hold, so the upcoming-actors bar at the top of the screen is the client's own derivation ([`src/order.rs`](src/order.rs), compiled into both ends), never a list the server sent. An audit checks it: every op feeds an [`OrderMirror`](src/mirror.rs), each turn the server actually opens is compared against the mirror's derivation and the panel prints the divergence count. It must read zero; the scripted run exits nonzero the moment it does not. The one exception is the snapshot a mid-battle joiner baselines from, which carries the standing round's order as data, because an initiative order was rolled against speeds as they stood at the boundary and a joiner has no way back to them. Derivation takes over at the next boundary.

Under initiative, a haste landing mid-round moves nothing until the boundary re-rolls and the projected tail of the bar visibly re-sorts when it lands. Under the delay queue the same haste rescales the target's **remaining** wait immediately and the bar re-sorts on the spot.

**Hovering an order previews it.** Before you commit, the act list grows a second, dimmed bar: the queue as it would stand if you gave that order, kill removed, slow pushed, smash's time cost charged to yourself. It is computed by the same shared derivation the audit runs on, with no crit rolled because a preview showing a crit would often be wrong. That makes the preview a second consumer of the shared derivation.

## What it says about `flow_control`

The initiative regime is composition over the shipped manager: one `RoundRobinTurnManager` per round, built from the re-rolled order at the boundary, its own turn notices going straight onto the wire as the audit line. No third `TurnManager` implementation was needed. The delay regime holds no manager at all: a linear scan and two integer rules, because a gauge has no passes for a manager to walk.

One deviation to know about before reusing the pattern ([`src/logic.rs`](src/logic.rs), `advance_turn`): the round boundary is decided **before** the last advance rather than read from `Advanced::PassClosed`. Advancing off the round's last actor would wrap the cursor, seat the first actor of the *old* order and emit its notice and only then report `PassClosed`; under a re-rolling boundary that notice names the wrong unit. `PassClosed` suits a continuous round-robin but arrives one turn too late for a pass that re-plans.

## Running it

```sh
./run-native.sh                          # host: a window plus the server; prints the join address
./run-native.sh --role client --connect ws://<host>:8303/ws
./run-native.sh --role headless          # the deployable server
./wasm-serve.sh                          # build the browser client and host it
cargo run -p turn_gauge --bin scripted   # no window, no socket: the audit as a gate
```

A lone commander gets the bot on the other side after a few seconds; a second human takes it if they arrive first. Extra joiners watch. A commander who lets their clock run out is acted for by the server and a commander who leaves hands their side to the bot, so a battle never stalls on a vacant chair.

## The panel

- **turns audited / diverged**: the mirror's result for every turn the server opened. The second number must read zero.
- **series / battle / round / turn** and how many chairs timed out.
- The act list across the top of the screen, beside the panel: the client's projection, current actor first. It is exact for the current actor and speculative after it, more so under the delay queue, since the moves to come are unknown and it assumes a standard action for each. The projection shifting when a haste lands is how this kind of turn order behaves, not an error.
