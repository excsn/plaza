# hit_scan

A 2-4 player top-down arena shooter with cover, a hitscan rifle and a slow rocket, where the server rewinds the world to decide every shot.

Every other networked example in this repository arbitrates between a player and a simulation: you predicted a cell, the server had another, and the gap is a correction. Nobody is on the other end of it. A shot is the first decision here with a **loser**, because granting the shooter the world they aimed at takes the shot away from a target who had already reached cover. Lag compensation is not a fix applied to a problem. It is a choice about who bears the disagreement, and this example is built so you can watch both sides of that choice move at once.

## Running it

```sh
./run-native.sh                              # host and play, serves the browser page too
./run-native.sh --role client --connect ws://host:8080/ws
./wasm-serve.sh 8080                         # headless, browser client on http://localhost:8080
./wasm-build.sh                              # rebuild the browser client only
cargo test -p hit_scan                       # every claim below, as a test
```

wasd or arrows to move, mouse aims, left click fires the rifle, right click fires a rocket. Touch devices get a stick and two buttons.

## What you are looking at

| On screen | Meaning |
|---|---|
| Solid circle | Where this client is drawing somebody |
| Hollow ring | Where the server **rewound** that target to when it judged the shot |
| Amber tracer | A hit that only landed because the server looked back |
| White tracer | A hit that landed in both worlds |
| Blue tracer | A hit the present would have allowed and the rewind took away |
| "shot from N ms in your past" | How far behind your own present the fatal decision was made |

The gap between the solid circle and the hollow ring is the whole example. It is what the target paid for the shooter's latency.

## The one decision everything follows from

`sim::server::resolve_shot` judges every shot **twice**: once against where the targets are now, and once against where the shooter saw them. The rewound world is authoritative, because refusing a shooter their own view is the same as telling them their aim does not work. The present world is not used to decide anything. It exists to produce the *verdict*, and the verdict is the only place the cost shows up.

### 1. A count of hits reports the shooter's experience and calls it fairness

So the panel reports four outcomes rather than two. `Plain` landed in both worlds and overruled nobody. `GrantedByRewind` missed against the present and hit once the server looked back. `DeniedByRewind` is the reverse, and is rarer than it sounds. `Miss` missed in both.

Beside them is the same set of events counted from the other end: **deaths behind cover**, asked of the present rather than of the shooter's screen. Could the victim, standing where they stand now, be seen from where the killer stands now? If not, they reached cover and were shot there anyway. Turning the rewind off does not make the game fair, it moves the unfairness onto the shooter, and both numbers are on the panel so that trade is visible instead of argued.

### 2. Peeker's advantage is arithmetic, and both terms are printed

`from_the_past_ms` is the shooter's rewind plus the delay the victim is rendering at. The falsifier is the render-delay slider: raise your own and your advantage as the peeker goes up while your defence gets worse, in the same frame. No single "netcode quality" number can express a trade that moves in two directions at once.

### 3. A rewind cannot reach past the history it reads

`HistoricalStateBuffer` retains by *count* and clamps to its oldest sample rather than refusing, so an unbounded rewind budget would resolve shots against a position the server no longer knows and report it as fact. `Rewind::Uncapped` is therefore bounded by `HISTORY_MS`, and the panel counts the shots whose honest rewind was longer than the cap allowed. A cap is not free, and saying how often it bit is the only way that shows.

### 4. Two weapons, two answers

The rifle is rewound. The rocket is a body the server owns and everybody watches arrive: no rewind, no compensation, no argument, and it is slower to land because of it. One weapon's fairness is a server policy and the other's is a client's patience.

## The ghost permission, enforced rather than declared

`ServerPolicy::allow_ghost` says whether a server hands a client frames stamped past that client's own render instant. Horde declares it and an honest client obeys; nothing stops a cheat client reading its queue anyway, so the drawing switch was never the control that mattered. For a shooter that is not cosmetic: a client holding unresolved frames can aim at where a target *will* be while the server rewinds to where that target *was*.

Turning the checkbox off here **withholds** instead. The formulation matters and only one of them works: delaying the send alone changes nothing, because the client's playout clock is derived from the stream and shifts with it, leaving the buffer depth identical to the millisecond. Enforcement has to withhold against the **declared timeline**, sending nothing whose timestamp is past `server_now - render_delay_ms`. The cost is on the panel next to it, because the unresolved window was the client's slack and removing it means every frame has to arrive inside one send interval.

## Two input schedules, never one

A held direction is a **level**: the newest value for a tick wins, and a lost one is corrected by the next. A shot is an **event**: dropping one is a trigger pull that never happened. They have different loss semantics, so mixing them in one queue forces one of them to be wrong, and `execute_due` keeping only the newest would silently eat every shot fired on a tick that also carried a direction. There are two `InputSchedule`s per seat and a test that pins it.

The client schedules its own prediction for the tick it *named* rather than applying it on the keypress. Doing otherwise runs the input a whole playout depth before the server will, and every frame then arrives as a correction. That is bomb_grid's lesson and it was rediscovered here the hard way: the first draft set the held direction on press, and the "latency alone produces no disagreement" test found 240 corrections in eight seconds.

## The measurement this made honest

`mean_render_error` across this repository compares a drawn position against server truth **now**, so it charges a client for a render delay it is taking deliberately. Every figure quoted at 10 Hz and above is inflated by roughly the delay times the speed.

The honest version compares against truth *at the instant being drawn*, which needs a truth history. The server already keeps one, because that is what a rewind reads. So the panel shows both numbers side by side and the difference between them is the delay you asked for. Two tests pin it: the honest figure is well under the naive one, and raising the render delay moves the naive figure while leaving the honest one alone.

## A slow link is refused, not merely slow

Past `playout_delay_ms + input_max_late_ticks * SIM_STEP_MS` every input names a tick that has already closed, so the fairness mechanism excludes the player it exists to protect. Such a link is refused at the door with **both numbers**, the measured one-way and the allowed one, so the refusal is checkable rather than a verdict. The measurement is `agent_rtt`, which is the server's own: a client's claim about its own latency is the one number worth lying about, and this one decides who gets in.

## How it is built

- `src/sim/` is the whole game with no sockets and no window: `rules.rs` is shared by both sides verbatim, `server.rs` is the authority, `client.rs` is a guess that gets corrected, and `world.rs` puts one of each behind a simulated link so a claim can be measured without a network.
- `src/net/` is the wire wrapper and adds no rules. What it adds over the harness is that the clock is estimated rather than shared, which matters more here than in a continuous game because every input names a tick.
- `render.rs` and `ui.rs` are bin-local: the panel is not part of the library surface.

## Notes

- Not in `default-members`: the macroquad dependency tree is large and a bare `cargo build` in `examples/` should not pay for it.
- The browser build is `--no-default-features --features web`. The default set pulls in tungstenite and actix, neither of which compiles to wasm.
- `static/hit_scan.wasm` is a build product and is gitignored.
- The map is a `const` in `types.rs`, which `build.rs` hashes into the protocol version. Moving a wall tells a stale browser bundle to reload rather than letting it argue about sight lines nobody else can see.
