# curtain_fire

A 1-4 player co-op bullet-hell shmup. Thousands of enemy bullets, a hitbox two and a half units across, and three different answers to the question of who is allowed to say you were hit.

Every other prediction example in this repository corrects a **position**: you drew a player a few pixels off, or on the wrong cell, and the fix is to move them. bomb_grid made the case that a lattice cannot hide its netcode because a wrong cell is a jump you can count. A shmup goes one further. The wrong answer here is not a position but a life, and there is no easing that, no rewinding it, and no apologising for it afterwards. So this is the example that asks a question none of the others do: **who decides you died?**

It carries a second, unrelated measurement because the shape suits it and nothing else here does. The enemy curtain is a closed-form function of the tick, so it costs one wave announcement and nothing else however many thousand bullets it becomes. Player fire depends on a human and costs bytes for ever. Both halves are on the same wire in the same second, so the panel can price them against each other rather than quoting two examples at one another.

## Running it

```sh
./run-native.sh                              # host and play, serves the browser page too
./run-native.sh -- --role client --connect ws://host:8080/ws
./wasm-serve.sh 8080                         # headless, browser client on http://localhost:8080
cargo test -p curtain_fire                   # every claim below, as a test
```

wasd or arrows to fly, space to fire. The white dot is your hitbox; the ship around it is decoration.

## The curtain is a function of the tick

`sim::curtain` is the entire enemy half of the game. It stores nothing, steps nothing, and sends nothing. A bullet's position is `spawn + velocity * age`, evaluated fresh every time it is asked for, never integrated. A whole wave is a pattern, a seed, a start tick and a handful of emitters: about two hundred bytes, which become several hundred bullets over the next fifteen seconds and cost nothing further.

The one thing about the curtain that is *not* derivable is when a gun stopped firing, because that depends on a player bullet, which depends on a human. So a kill sends one small `ArmDown` op naming the tick, and everything downstream of it stays derivable: both ends cut that emitter's output at the same instant, for ever.

The constraint this puts on the code is absolute and worth stating, because breaking it is silent. **No accumulation anywhere.** An integrated curtain drifts apart on two machines and nothing would ever notice, because nothing about it is compared: there is no snapshot to disagree with and no digest to fail. A test evaluates the same tick after two different histories and asserts the field is identical.

## Who may say you died

The rule is a server policy on the wire, selectable in the panel, and each answer has the number that condemns it.

**The server decides.** Correct, and the client is not allowed to act on a contact it can plainly see. The player watches themself keep flying for a round trip after they already know they are dead.

**The ship decides.** What shipped co-op shmups actually do, and it feels perfect, because it is judged against exactly what the player saw. Trivially cheatable: the panel has a switch that makes one seat stop declaring, and that seat becomes immortal. The interesting number is not that it works but what it costs to see: the server derives the same curtain, so counting the contacts nobody owned up to is one comparison against an evaluation it was already doing. A silent seat's count climbs without bound and an honest seat's does not.

**The ship declares and the server checks.** The server recomputes the same field the client dodged, at the tick that was named, against where that ship actually was then. Fair and checkable, and only possible because the curtain is a function of the tick: with a streamed field there would be nothing to recompute.

### The finding this produced

The example was planned around "the server deciding kills you a round trip after you dodged", and that is not what happens. Because the curtain is derivable, **the client is never the last to know**. It computed the same field the server did and saw the contact on the same tick. The rule does not decide who finds out, it decides **who is allowed to act on it**, and being made to fly a ship you know is already dead is worse than not knowing. The counter is named `flown_while_dead_ticks` for that reason.

## What the wire carried

Two numbers side by side, over the same traffic:

- **The derived half**: bytes per enemy bullet on screen, which falls towards nothing as the curtain thickens.
- **The streamed half**: bytes per player bullet, which does not move.

seed_defense established that sending causes instead of consequences works. What it could not do is put a price on it, because it had no underivable half to compare against. This does.

### And a measurement nobody had taken

`IMPROVEMENTS` gates float quantization, bit packing and numeric variant tags on one number: the share of a frame that is the names of its variants. It had never been measured, and the answer is not the one `MsgPackCodec`'s own documentation implies. Compact MessagePack makes **struct fields** positional; enum variants are still written out as strings. On this example's traffic the share is real and on the panel.

The other half of the finding is why this stays a measurement rather than a rule: a tag is a fixed cost, so it dominates a stream of small events and disappears into a large frame. Anyone reaching for numeric tags should first find out which of the two they have. Both directions are pinned by tests.

## What is shared and what is not

The curtain code is called by the server, by every client, and by the offline harness, with the same arguments. A test asserts that for every wave a client knows about, its bullets are exactly the server's, position by position. Agreement is checked **per wave** rather than in total, because a wave announcement takes a one-way trip like anything else and a client legitimately has fewer waves than the server for a moment after each one starts.

A joiner is welcomed with every wave already in flight. Without that it derives an empty field and flies through a curtain it cannot see, and nothing in the frames it is receiving would ever say so. That is the one failure mode a derived field has that a streamed one does not, and it has its own test.

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
