# last_word

A two-seat counterspell duel in the listen-server shape (native window, wasm page, headless server, a bot for the empty seat) about stacked response windows, that is, *when a spell may be cast*: a cast opens a window for the opponent, a response stacks on top of what it answers and nothing resolves until both duelists pass in succession.

## The window machine

Priority is a token exactly one duelist holds. A cast puts the spell on the stack and hands the token across. A pass hands it back. **Two passes in succession resolve the top** and every resolution returns the token to the turn's owner with the pass count cleared, so nothing resolves before both duelists have had the chance to respond. The machine has no way to skip a window. A cast spends every standing pass, which is what makes answering your own spell a real move. This is the first flow structure in the workspace that is not flat: a turn contains windows, a window contains casts and each cast reopens the window it interrupted.

Four spells, no deck, no hidden information: **Bolt** (4 damage, sorcery-speed: your own turn, quiet stack only), **Jolt** (2, instant), **Mend** (+3, instant) and **Counter**, which resolves by removing the spell below it, so that spell never resolves. Both tempo pools refill to `min(turn, 10)` every turn, the responder's included, because otherwise the defender could not afford to fight a counter war. The accounting the IDEAS entry asked for is on the panel: every cast and every resolution opens a window (`windows >= casts + resolutions`, the excess being lone passes handing the token back), the deepest stack is printed and the counter-war test pins the LIFO order card by card, down to the caster of the topmost counter getting the last word.

## Playing it

```sh
./run-native.sh                          # host: window plus server; prints the join address
./run-native.sh --role client --connect ws://<host>:8305/ws
./run-native.sh --role headless
./wasm-serve.sh                          # browser client on the same port
cargo run -p last_word --bin scripted    # no window: the accounting as a gate
```

The stack is the middle of the screen, bottom up, the top marked "resolves next"; a counter visibly lands on the thing it answers. When the gold outline is on your totem the window is yours: the spell row along the bottom grays out what is illegal right now (a Bolt off-turn, a Counter with nothing to answer, anything you cannot afford) and PASS (or Space) declines. Each window has a 7-second clock and letting it run out passes.

## Beyond the stack

A resolution's ledger reads `casts == resolutions + countered`: a countered spell fizzles and never resolves. The first draft of this example's own test got that wrong. The termination rule alone (consecutive passes close a window) is what an auction's "going once" would exercise. The stack is needed for the rest: LIFO order, reopened passes and priority returning to the turn's owner.
