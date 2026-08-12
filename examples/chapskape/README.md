# ChapsKape

A square of countryside you click at: trees to chop, rocks to mine, shoals to fish, a fire to cook on, a pack to carry it in, and brutes that hit back, on a tick slow enough to see.

```sh
./run-native.sh            # play it, and host it, in one window
./wasm-serve.sh            # the same thing in a browser, on port 8302
cargo test -p chapskape    # the findings, as assertions
```

**Click somewhere to go there.** Click a tree, a rock or a shoal to walk over and work it; click a brute to fight it; click something lying about to pick it up. **R** runs, **space** stops, and clicking a square of your pack uses it while shift-clicking drops it. Right-drag or the arrow keys swing the camera. `--bots N` sets how many of the world's own it seats.

The world is 4037 props over 192x192 squares, and it is inhabited before you arrive: two dozen bodies chopping, fishing, cooking and fighting. Two minutes into a headless run with nobody connected, a joiner arrives to six people in view and ten things lying on the ground.

The thing worth doing is dropping something and watching the ring around it. It is yours alone for fifty ticks, and **nobody else is even told it is there** until the timer runs out.

## What it is for

Set this beside `spacemo`, which absorbs no latency at all and must predict every frame, and `gow_3d`, which absorbs a cast bar's worth and gets away with sending nothing back. This one asks what is left when the input itself is a **destination**.

| | the input is | what a round trip costs |
| --- | --- | --- |
| spacemo | a stick, 60 times a second | a prediction and a reconciler |
| gow_3d | a held direction, 30 times a second | nothing, the cast bar is longer |
| poketo | one step, when you take it | nothing, the step has a duration |
| chapskape | a place, when you decide to go there | nothing, and there is nothing to reconcile either |

## A destination is the cheapest input there is

`cargo test -p chapskape --test wire_cost -- --nocapture`

```
  a journey is 22 squares, which is 13.3 seconds of walking.

                     input  ops/journey    bytes/journey
            a place (this)            1               13
  a held direction at 30Hz          398             4774
   a step, as poketo sends           22              133
```

A journey here is as far as a player can see, because a click is something they aimed at.

The byte count is the small half. The large half is that **a destination cannot be wrong the way a claimed position can.** gow_3d spends a validator, a tolerance constant and a rejection counter to police a client that says where it is. This spends none of them, because the client never asserted a position: it asked, and the answer was a route it could work out for itself.

## The client draws the route before the server has heard the click

Terrain, walkability and every prop in the world are derived from one seed on both ends, so the pathfinder runs on both ends too. Click, and the body sets off on the frame the mouse went down. The server hears about it a round trip later and expands the same square with the same rule.

`cargo test -p chapskape --test two_ends -- --nocapture`

```
  24 squares walked for one op, 24 confirmed, 0 diverged
  the walk took 14400 ms before the axe moved, on one op
```

Which concentrates the whole determinism surface into one place: **the tie-break.** Two routes of equal length are equally correct and only one of them is the one the server picked, so anything that leaves the choice between them to an implementation detail is a divergence waiting for the first symmetric stretch of grass. Three things make that impossible here rather than unlikely:

- The open set is ordered on `(f, h, seq)` where `seq` counts pushes, so ties fall to whichever square was reached first and never to heap internals.
- Every table in the search is a dense array indexed by square. **There is no hash map in the search**, so there is no iteration order to depend on.
- Neighbours are visited in one fixed order, cardinals before diagonals, which is also what makes a route look like something a person would walk.

`the_tie_break_is_pinned_rather_than_incidental` asserts *which* of several equal routes comes out, on purpose. If the neighbour order or the open set's ordering changes, it fails, which is the point: both are part of the protocol in everything but name.

## The check is a route check, not a position check

The client acts on a click immediately and the server acts on it a tick and a round trip later, so the two are permanently out of phase by design. Asking whether they are on the same square right now would count that phase offset as an error and bury the thing the counter is for. Asking whether the server is walking **the squares the client already drew** is the question with a right answer, and the answer is yes every time.

`route diverged` on the panel should read zero for a whole session. A number climbing there means the rule stopped being one rule.

## A still world is a different relevance problem from a moving one

Four thousand props exist and perhaps one changes a tick. Every relevance path in this tree is built for movers: a grid query rebuilt every tick and a diff that pays for the whole set whether or not anything in it moved.

```
  257 props inside a 24-square view, 6 of them out
  8 props out, forty ticks, one viewer:

    every tick   320 entries sent,  320 if every frame carried them
    on change      8 entries sent,  320 if every frame carried them
```

Measured on a frame, in a world with two dozen bodies working in it:

```
       props bytes/frame bytes/second of that, props
  every tick        588          980          73.4
   on change        520          867           6.4
```

Two things make the cheap mode possible, and neither is the diff.

**A prop's id is its square.** Nothing ever sends where a tree is, because both ends derive the props from the map. What travels is that one of them is out.

**A stable state can be sent once.** `ready_at` is an absolute tick rather than a countdown. A countdown differs on every tick, so a client that wanted one would have to be told every tick whether anything had happened or not, and there would be no change-only mode to have.

The dial is on the panel and both modes live in one build, because the comparison is the deliverable rather than either mode on its own.

**What the cheap mode costs is a second piece of client code.** Under `every tick` a frame is the whole visible set, so absence means a prop is standing again. Under `on change` absence means nothing happened, and a prop coming back has to be said out loud with a zero, or a client draws a stump for the rest of the session. `a_prop_that_comes_back_is_said_out_loud_in_either_mode` runs both.

## An audience can be a game rule

Drop something and it is yours alone for fifty ticks, then it belongs to whoever walks past. The audience of that entity is decided by a **deadline**: not by distance, which `plaza_server_utils::relevance` answers, and not by a chosen set, which `subscription` answers.

The rule is enforced where it matters rather than where it is easy. A client who may not take an item **is not told it is there**, so there is nothing on screen to click and be refused about. `a_dropped_item_reaches_its_owner_and_nobody_else` runs two clients and checks both sides of that.

## A pack is a stream that exists for one client

`fog_skirmish` filters a shared world per viewer, which is a different thing: the fog hides something that is there for everybody. Nothing in a pack is filtered, because nobody else's world contains it.

```
  a frame carrying the pack: 85 bytes
  a frame not carrying it:   46 bytes
  sent 2 times in 40 ticks
```

Sent when it moves, so standing in a field is free. The instant worth watching is the crossing: a drop turns private state into world state, and a pickup turns it back.

## The tick is vocabulary rather than something to hide

Every other example in this tree spends effort concealing its tick from the person playing. At 600ms a player can see it, count it and act against it, and the interface shows it rather than smoothing it away. The panel drags it down to 50ms, which is where a design decision turns back into a netcode problem:

```
   tick ms  bytes/frame   bytes/second
       600          511            852
       300          512           1708
       150          360           2401
        50          225           4495
```

A shorter tick makes each frame a little smaller, because less happens in one, and the bill several times larger anyway. Everything this example says about free round trips is said at six hundred milliseconds.

The host wakes every 50ms regardless and a game tick is a **budget drawn down** rather than a wake-up answered, which is what makes the length a dial instead of a constant.

## The world is a rule, and so is the pathfinder over it

Nothing about the landscape crosses the wire: heights, ground, walkability and props are all functions of a square. That is the same trick gow_3d plays with its hills and poketo with its tile map, and here it buys something neither of those needed, since the client can expand a destination before the server has heard the question.

It is derived **once**, though, and that is arithmetic rather than principle. A search settles thousands of squares and asks each whether it can be walked on; asking three octaves of noise and four neighbours every time turns one click into a million hashes. Both ends build a table from the rule at startup and read it after that. The lib tests went from 70 seconds to 1 second on that change alone.

## What the world does while nobody is watching

`cargo test -p chapskape --lib the_world_gets_on -- --nocapture`

```
  after 400 ticks: 126 gathered, 131 blows, 30 felled, 3682 experience,
                   25 levels, 55 props used up, 15 on the ground
```

The world's own live the same loop a player does, through the same ops a client sends, so nothing downstream knows the difference. Chop until the pack is heavy, set light to the logs, catch fish, cook them on the fire, go and fight something, eat when it hurts, start again.

That loop being **closed** is the discipline that keeps the content from running away. Every piece of it sits on the circle or it does not go in, which is a smaller world than five skills that each end in a number going up.

## Seams worth knowing about

Every one of these is a place where both halves were individually correct.

- **A hash map's order reached the random stream.** `think()` collected the wandering bodies straight out of a `HashMap`, and every one of them then drew from one shared xorshift, so the same tick run twice was not the same tick. The fix is a sort; the reason it is not tidiness is that the order decides who wanders where. `a_tick_is_the_same_tick_when_it_is_run_again` is what found it.
- **A client is never in its own audience.** `You` exists for that reason, and gow_3d shipped the other way round: a client that read itself out of the list of other people read nothing at all, and every key press was silent.
- **A refusal a player cannot read is a broken key.** Every one is named on the wire and said in words, once. `NeedsLevel` carries the skill and the level, because "nothing happened" and "you need woodcutting 8" look identical from the outside.
- **A respawn is the one square that arrives rather than departs.** A counter rather than a flag, so the client applies the move exactly once however many frames repeat it, and a dropped frame is caught by the next.
- **A diagonal needs both of its sides open**, or a body walks through the join of two walls. It is the one pathfinding bug a player notices immediately and cannot unsee.
- **A walled-off click still means something.** The search returns the best partial route rather than nothing, because standing still is a worse answer than setting off, and *which* partial is decided by the same total order, so giving up is as reproducible as succeeding.
- **A pack fills the first free square rather than the end**, or a player who eats from the middle watches their pack grow past its own last square and then refuse an item it plainly has room for.
- **The draw batch is bounded by the buffer, asked at every push.** macroquad's batcher clamps at 10000 vertices and 5000 indices, warns once, and draws the front of the buffer, so a scene past it is quietly missing rather than broken. Counting bodies was gow_3d's bug and it is not repeated here.

## Layout

| file | what is in it |
| --- | --- |
| `world.rs` | the map, its props, and the table both ends build from the rule |
| `path.rs` | the search, and the tie-break that is the whole determinism surface |
| `skills.rs` | five skills, one closed loop, and the curve |
| `pack.rs` | twenty-eight squares that exist for one client |
| `zone.rs` | the moving half, the still half, and the ground between them |
| `bots.rs` | the world's own, living the loop |
| `protocol.rs` | the wire, and nothing that is not on it |
| `logic.rs` | the tick, and the frame it produces |
| `state.rs` | what each viewer has already been told about the still world |
| `net/` | both ends of the wire |
| `render.rs`, `ui.rs`, `main.rs` | the countryside, the pack, and the click |
| `tests/two_ends.rs` | both sides run together, which is where the route claim can be checked |
| `tests/wire_cost.rs` | what a place costs against a held key |

## The name

"RuneScape" is a live trademark and this repo is public, so nothing is borrowed with the shape: no place names, no character names, no art, no skill list copied wholesale, no numbers lifted from anywhere. Generic English words for generic activities, in a world that is 192 squares across and forgets itself when the process ends.
