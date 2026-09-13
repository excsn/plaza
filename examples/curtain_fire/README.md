# curtain_fire

A 1-4 player co-op bullet-hell shmup with thousands of enemy bullets and a hitbox two and a half units across. It has three different rules for who decides that you were hit.

Every other prediction example in this repository corrects a **position**: you drew a player a few pixels off or on the wrong cell and the fix is to move them. bomb_grid showed that a lattice cannot hide its netcode because a wrong cell is a jump you can count. In a shmup a wrong answer costs a life. A death cannot be eased or rewound afterwards, so this example is about **who decides that you died**.

It also carries a second, unrelated measurement, because no other example here has the right shape for it. The enemy curtain is a closed-form function of the tick, so each wave costs one announcement however many thousand bullets it produces. Player fire depends on a human, so it keeps costing bytes. Both are on the same wire at the same time, so the panel can compare their costs directly instead of comparing two different examples.

## Running it

```sh
./run-native.sh                              # host and play, serves the browser page too
./run-native.sh --role client --connect ws://host:8080/ws
./wasm-serve.sh 8080                         # headless, browser client on http://localhost:8080
cargo test -p curtain_fire                   # every claim below, as a test
```

wasd or arrows to fly, space to fire. The white dot is your hitbox; the ship around it is decoration.

## The curtain

`sim::curtain` is the entire enemy half of the game. It holds no state and is never stepped or sent. A bullet's position is `spawn + velocity * age`, evaluated fresh every time it is asked for and never integrated. A whole wave is a pattern, a seed, a start tick and a handful of emitters: about two hundred bytes, which become several hundred bullets over the next fifteen seconds with no further traffic.

The one thing about the curtain that cannot be derived is when a gun stopped firing, because that depends on a player's bullet. So a kill sends one small `ArmDown` op naming the tick and everything after it can still be derived: both ends stop that emitter's output at the same tick from then on.

The curtain code must never accumulate. Nothing flags it if it does. An integrated curtain drifts apart on two machines and nothing would notice, because nothing about it is compared: there is no snapshot to disagree with and no digest to fail. A test evaluates the same tick after two different histories and asserts the field is identical.

## Who decides a death

The rule is a server policy sent on the wire and selectable in the panel. Each rule has a number on the panel that shows its weakness.

**The server decides.** This is correct, but the client is not allowed to act on a contact it can see. The player keeps flying for a round trip after they already know they are dead.

**The ship decides.** Shipped co-op shmups do this and it feels right, because it is judged against exactly what the player saw. It is easy to cheat: the panel has a switch that makes one seat stop declaring and that seat becomes immortal. The server can detect this cheaply. It derives the same curtain, so counting the contacts nobody declared is one comparison against an evaluation it already does. A silent seat's count keeps climbing and an honest seat's does not.

**The ship declares and the server checks.** The server recomputes the same field the client dodged, at the tick that was named, against where that ship actually was then. This is fair and checkable. It only works because the curtain is a function of the tick; with a streamed field the server would have nothing to recompute.

### When the client knows

The example was planned around "the server deciding kills you a round trip after you dodged" and that is not what happens. Because the curtain is derivable, the client computes the same field the server does and sees the contact on the same tick. The rule only decides **who is allowed to act on it**. Being made to fly a ship you know is already dead is worse than not knowing, so the counter is named `flown_while_dead_ticks`.

## What the wire carried

Two numbers side by side, over the same traffic:

- **The derived half**: bytes per enemy bullet on screen, which falls towards nothing as the curtain thickens.
- **The streamed half**: bytes per player bullet, which does not move.

seed_defense showed that sending only the causes of the state works. It had no half that had to be streamed, so it could not compare the two costs. This example has both halves.

### The share of a frame that is variant names

`IMPROVEMENTS` makes float quantization, bit packing and numeric variant tags depend on one number: the share of a frame taken up by variant names. `MsgPackCodec`'s documentation says compact MessagePack keeps the names and that short names are therefore worth having, but it does not give the share, which is what the backlog item is waiting on. This example measures the share on its own traffic and shows it on the panel.

It stays a measurement and not a rule because a tag is a fixed cost: it dominates a stream of small events and is negligible in a large frame. Before switching to numeric tags, find out which kind of traffic you have. Tests pin both cases.

## What is shared and what is not

The curtain code is called by the server, by every client and by the offline harness, with the same arguments. A test asserts that for every wave a client knows about, its bullets are exactly the server's, position by position. Agreement is checked **per wave** rather than in total, because a wave announcement takes a one-way trip like anything else and a client legitimately has fewer waves than the server for a moment after each one starts.

A joiner receives every wave already in flight when it joins. Without them it derives an empty field and flies through bullets it cannot see. Nothing in the frames it receives would show the problem. A streamed field cannot fail this way and this case has its own test.

## How it is built

- `src/sim/curtain.rs` is the file to read first: the whole enemy half, holding no state.
- `src/sim/server.rs` is the authority; `judge_deaths` and `declare` are the three rules.
- `src/sim/client.rs` derives the curtain and decides whether it believes it has been hit. It returns a declaration rather than sending one, which is what lets the offline harness run the identical code.
- `src/sim/protocol.rs` carries `wire_cost`, the byte accounting and the numerically-tagged mirror of the op enum.
- `src/sim/world.rs` puts one server and N clients behind a simulated link.

## Notes

- Not in `default-members`: the macroquad dependency tree is large.
- The browser build is `--no-default-features --features web`.
- `static/curtain_fire.wasm` is a build product and is gitignored.
- `rmp-serde` is a direct, non-optional dependency here. Pricing the wire is one of this example's two measurements, so it has to be able to encode a message whether or not a socket is compiled in.
