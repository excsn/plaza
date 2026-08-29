# last_word

A stack of response windows. A two-seat counterspell duel in the listen-server shape (native window, wasm page, headless server, a bot for the empty seat) whose whole subject is *when a spell may be spoken*: a cast opens a window for the opponent, a response stacks on top of what it answers, and nothing resolves until both duelists pass in succession.

## The window machine

Priority is a token exactly one duelist holds. A cast puts the spell on the stack and hands the token across; a pass hands it back; **two passes in succession resolve the top**, and every resolution returns the token to the turn's owner with the pass count cleared, so a resolution can never be slipped past anybody: the machine cannot express a skipped window at all. A cast spends every standing pass, which is what makes answering your own spell a real move. This is the first flow structure in the workspace that is not flat: a turn contains windows, a window contains casts, and each cast reopens the window it interrupted.

Four spells, no deck, no hidden information: **Bolt** (4 damage, sorcery-speed: your own turn, quiet stack only), **Jolt** (2, instant), **Mend** (+3, instant), and **Counter**, which resolves by removing the spell below it, which therefore never resolves at all. Both tempo pools refill to `min(turn, 10)` every turn, the responder's included, because a counter war is only a war if the defender can afford to fight it. The accounting the IDEAS entry asked for is on the panel: every cast and every resolution opens a window (`windows >= casts + resolutions`, the excess being lone passes handing the token back), the deepest stack is printed, and the counter-war test pins the LIFO order to the card: last in, first out, and the caster of the topmost counter gets the last word.

## Playing it

```sh
./run-native.sh                          # host: window plus server; prints the join address
./run-native.sh --role client --connect ws://<host>:8305/ws
./run-native.sh --role headless
./wasm-serve.sh                          # browser client on the same port
cargo run -p last_word --bin scripted    # no window: the accounting as a gate
```

The stack is the middle of the screen, bottom up, the top marked "resolves next"; a counter visibly lands on the thing it answers. When the gold outline is on your totem the window is yours: the spell row along the bottom grays out what is illegal right now (a Bolt off-turn, a Counter with nothing to answer, anything you cannot afford), and PASS (or Space) declines. Windows run on clocks, 7 seconds to respond, and silence passes.

## What it teaches beside the stack

A resolution's ledger reads `casts == resolutions + countered`: a countered spell fizzles, it never resolves, and the first draft of this example's own test got that wrong, which is exactly the bookkeeping subtlety the genus carries. The termination rule alone (consecutive passes close a window) is what an auction's "going once" would exercise; what earns the stack is everything above it, LIFO, re-opened passes and priority returning home.
