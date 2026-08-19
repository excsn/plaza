# ant_farm

A colony too big to send, watched through panes that are not. Every client asks for a window onto the board and receives only the cells that window touches, packed once per cell however many watchers share them. The name is the claim: an ant farm is a narrow pane over a colony that does not fit in it.

## Running it

```sh
./run-native.sh                       # stand the colony up and open the observer window on it
./run-native.sh --ants 1000000        # the headline population
./run-native.sh --connect <host:port> # watch a server already running elsewhere
./run-probe.sh --watchers 8           # the traffic fleet, no window
```

The observer window pans by drag or WASD and zooms on the wheel, drawing exactly what the wire carried and nothing else. It is a client like any probe, so panning is just a `Window` op and the server packs whatever the new pane touches. Its panel carries the server's phase timings, the controller's own tick accounting, the wire numbers and a live **ants** slider that resizes the colony while you watch; the numbers arrive as a `Stats` op once a second, so the readouts are the server's accounting rather than a model of it.

The deployable is the server alone, `cargo run --release -p plaza_example_ant_farm`, which is what `--connect` watches; it runs the colony on UDP at `0.0.0.0:4747` and takes `--ants`, `--sites`, `--seed` and `--bind`. The probe takes `--connect`, `--watchers`, `--half`, `--drift`, `--secs` and `--draw`, which renders watcher zero's pane as ASCII density. The `view` feature keeps macroquad out of the server binary, which matters for a headless Linux box that has no GL to link.

## The panel

Headless, the server prints the same snapshot it broadcasts, one line a second: mean and worst milliseconds for each phase of the tick, because which phase owns the frame is the finding this example exists to produce.

```
ants 50000 | watchers 1 | step 0.5ms w0.7 | rebuild 0.2ms w0.3 | publish 0.13ms w0.18 (286 cells) | assemble 0.02ms w0.03 | tick 0.8ms w2.4 | udp 245 pkt/s 0.25 MB/s busy 6.6ms/s
```

`step` moves every ant, `rebuild` buckets them by cell, `publish` packs each occupied cell at least one pane wants, `assemble` deals payloads out per watcher, `tick` is `ControllerStats`'s own mean and lifetime worst across every input (the reference the phase timings answer to), and the wire block is the send path's accounting: packets, bytes and how long the process spent inside send calls. The probe prints the receiving side: packets, ants and cells per second, plus the worst tick gap it saw, which is what loss costs here.

## The wire

One datagram is one message. Cell records are `[cell u16][count u16][dx u8, dy u8]*`, self-delimiting, concatenated into payloads that never exceed a 1200-byte MTU; a crowded cell splits into several records rather than outgrowing a datagram. Every payload carries complete cell state, so a lost datagram costs freshness and never correctness, and nothing needs a baseline, a retransmit or a fragment header. The one op that must arrive, the `Welcome`, is resent through `plaza_server_utils::oneshot::Pending` until the client says it landed.

The transport is the seam `foreign_soil` proved: an adapter over `TransportSession`, `ConnectionManager` and the frame module, with a connection being this adapter's own invention (first datagram in, idle sweep out). Two findings from wiring a real fan-out through it:

- **The controller coalesces neighbouring same-target ops into one envelope.** Right on a stream, fatal on a datagram link: a watcher's whole pane merged into one 17 KB frame that nothing could send. The tick therefore hands `Cells` payloads to the session itself, one message per payload, and only the small ops ride the controller's output.
- **A join and the ops that caused it race.** The first `Window` op can reach the logic before `AgentJoined` does, so joining must not clobber a watcher the op already created.

## Two shapes of visibility

Delivery here needs no per-entity tracking at all, which is itself the point: resending whole cells makes "who entered, who left" a client-side question. But games that stream entities need the answer server-side, and there are two shapes: `VisibilitySet`, a dense bitset diffed word at a time, O(population) per watcher per tick however small the pane; and a sparse sorted set diffed against only what the grid query returned, O(visible). The crossing between them is a number, not an opinion: the harness

```
cargo run --release -p plaza_example_ant_farm --example vis_scale
```

sweeps population x watchers with both shapes observing identical queries and prints microseconds per tick for each, with the query cost in its own column so neither shape hides it.

## The XDP arm, Linux only

Built with `--features xdp` on Linux, `--xdp <iface>` switches the send path from `sendto` per datagram to an AF_XDP TX ring; macOS and every build without the feature use plain UDP, and a Linux build where XDP setup fails (no capability, no driver support) falls back to UDP and says so. The panel names the live arm.

TX only, deliberately: transmit needs no BPF redirect program, so inbound traffic keeps arriving at the ordinary UDP socket, whose address the hand-built frames name as their source. What kernel bypass costs is spelled out in `src/send/frame.rs`: Ethernet, IPv4 and UDP headers written by hand, checksums included, and a `--xdp-dst-mac` for the next hop because bypassing the stack also bypasses ARP. It needs CAP_NET_ADMIN and honest numbers need a NIC whose driver does zero-copy AF_XDP; copy mode works but measures the fallback, not the idea.

At this example's default scale the wire is not the ceiling and the tick is, which is exactly why the arm exists as a measured choice rather than a default: the crossover where per-packet send cost would own the frame is a curve to draw, not an assumption to build on.
