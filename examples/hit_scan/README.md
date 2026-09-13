# hit_scan

A 2-4 player top-down arena shooter with cover, a hitscan rifle and a slow rocket, where the server rewinds the world to decide every shot.

In every other networked example in this repository the disagreement is between a player and the simulation: you predicted a cell, the server had another and the gap is a correction. No other player is affected. A shot can have a **loser**: if the server grants the shooter the world they aimed at, a target who had already reached cover still gets hit. Lag compensation decides which of the two players bears the disagreement. The panel shows what it costs each side at the same time.

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

The gap between the solid circle and the hollow ring comes from the shooter's latency: the target was judged at a position it had already left.

## How a shot is judged

`sim::server::resolve_shot` judges every shot **twice**: once against where the targets are now and once against where the shooter saw them. The rewound world decides the shot, because refusing a shooter what they saw makes their aim feel broken. The present world does not decide anything. It is used only to produce the *verdict*, which is where the cost to the target is recorded.

### 1. Four shot outcomes and deaths behind cover

The panel reports four outcomes. `Plain` landed in both worlds and overruled nobody. `GrantedByRewind` missed against the present and hit once the server looked back. `DeniedByRewind` is the reverse and is rarer than it sounds. `Miss` missed in both.

Beside them is the same set of events counted from the target's side: **deaths behind cover**, checked against the present instead of the shooter's screen. For each kill it asks whether the victim, at their current position, is visible from the killer's current position. If not, the victim reached cover and was shot there anyway. Turning the rewind off does not remove this unfairness. It moves it onto the shooter. Both numbers are on the panel.

### 2. Peeker's advantage

`from_the_past_ms` is the shooter's rewind plus the victim's render delay and the panel prints both terms. To check it, raise your own render delay with the slider: your advantage as the peeker goes up and your defence gets worse in the same frame.

### 3. The rewind cap

`HistoricalStateBuffer` retains by *count* and clamps to its oldest sample instead of refusing, so an unbounded rewind would resolve shots against a position the server no longer has and report the result as real. `Rewind::Uncapped` is therefore bounded by `HISTORY_MS`. The panel counts the shots that needed a longer rewind than the cap allowed.

### 4. The rifle is rewound and the rocket is not

The rocket is a body the server owns and every client watches it travel. It gets no rewind and is slower to land because of it. How fair the rifle is depends on the server's rewind policy. The rocket needs no policy: the shooter waits for it to land.

## Enforcing the ghost permission

`ServerPolicy::allow_ghost` says whether a server hands a client frames stamped past that client's own render instant. Horde only declares it: an honest client obeys, but a cheat client can read its queue anyway, so the drawing switch does not actually prevent anything. In a shooter this matters: a client holding unresolved frames can aim at where a target *will* be while the server rewinds to where that target *was*.

Here, turning the checkbox off makes the server **withhold** those frames. Delaying the send does not work on its own: the client's playout clock is derived from the stream and shifts with it, so the buffer depth stays the same to the millisecond. The server has to withhold against the **declared timeline** and send nothing whose timestamp is past `server_now - render_delay_ms`. The panel shows the cost next to the checkbox: the unresolved window was the client's slack, so without it every frame has to arrive within one send interval.

## Two input schedules per seat

A held direction behaves like a level: the newest value for a tick wins and a lost one is replaced by the next. A shot is an event: if one is dropped, that trigger pull never happened. Because they handle loss differently, one queue cannot serve both. `execute_due` keeps only the newest input, so a shared queue would silently drop every shot fired on a tick that also carried a direction. Each seat has two `InputSchedule`s and a test pins that.

The client applies its own input to its prediction at the tick the input *named*, not on the keypress. Applying it on the keypress runs the input a whole playout depth before the server does and every frame then arrives as a correction. bomb_grid found this first. The first draft here set the held direction on press and the "latency alone produces no disagreement" test found 240 corrections in eight seconds.

## Render error at the drawn instant

Elsewhere in this repository `mean_render_error` compares a drawn position against server truth **now**, so it counts a deliberate render delay as error. Every figure quoted at 10 Hz and above is inflated by roughly the delay times the speed.

This example also compares against truth *at the instant being drawn*, which needs a history of truth. The server already keeps one for rewinds. The panel shows both numbers side by side and the difference between them is the render delay you chose. Two tests pin it: the drawn-instant figure is well under the naive one and raising the render delay moves the naive figure but leaves the drawn-instant one unchanged.

## Refusing a link that is too slow

Past `playout_delay_ms + input_max_late_ticks * SIM_STEP_MS` every input names a tick that has already closed, so lag compensation cannot work for that player. The server refuses such a link when it connects and reports **both numbers**, the measured one-way delay and the allowed one, so the player can see why. The measurement is `agent_rtt`, which the server takes itself. The client's own latency report is not used, because a client could lie to get in.

## How it is built

- `src/sim/` is the whole game with no sockets and no window: `rules.rs` is shared by both sides verbatim, `server.rs` is the authority, `client.rs` predicts and gets corrected and `world.rs` puts one of each behind a simulated link so a claim can be measured without a network.
- `src/net/` is the wire wrapper and adds no rules. The difference from the harness is that each client has to estimate the server's clock, which matters more here than in a continuous game because every input names a tick.
- `render.rs` and `ui.rs` are bin-local: the panel is not part of the library surface.

## Notes

- Not in `default-members`: the macroquad dependency tree is large and a bare `cargo build` in `examples/` should not pay for it.
- The browser build is `--no-default-features --features web`. The default set pulls in tungstenite and actix, neither of which compiles to wasm.
- `static/hit_scan.wasm` is a build product and is gitignored.
- The map is a `const` in `types.rs`, which `build.rs` hashes into the protocol version. Moving a wall changes the version, so a stale browser bundle is told to reload instead of playing on a map nobody else has.
