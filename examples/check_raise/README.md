# check_raise

A turn order the players keep re-opening. Fixed-limit poker at four chairs in the listen-server shape (native window, wasm page, headless server, the house filling empty seats), built for the **betting round**: the one turn structure in this workspace that closes by consensus instead of exhaustion.

## The round that will not say when it ends

A street's asks live in a queue. A raise **rebuilds it**, everyone active except the raiser owed an answer again, so the finish line moves mid-round and the round ends only when the queue drains: action has returned to the last aggressor with nobody owing. Folding removes a seat mid-queue. An **all-in seat is the state no shipped turn manager has a word for**: present, invested, eligible for everything it covered, and never asked again; each rebuild counts what it walked past. Preflop the big blind is the street's opening bet and keeps its option when everyone only calls, and the button rotates each hand, so every pass of the order starts somewhere else, pass-plans arriving as data.

Fixed-limit keeps the arithmetic honest: bets of 2 on the early streets and 4 on the late ones, a cap of four bets per street, blinds 1 and 2. Side pots settle in layers over each seat's total contribution, folded money staying in, and the settlement (`logic::settle`) is a pure function whose tests pin conservation to the chip: a short all-in wins only what it covered, layers above go to the best hand still eligible, odd chips land nearest the button. Hole cards ride only their seat's view until a showdown turns the live hands face up, card_table's per-recipient seam carrying a game where the secret is the whole economy.

## Playing it

```sh
./run-native.sh                          # host: window plus server; prints the join address
./run-native.sh --role client --connect ws://<host>:8306/ws
./run-native.sh --role headless
./wasm-serve.sh                          # browser client on the same port
cargo run -p check_raise --bin scripted  # no window: the reopening ledger as a gate
```

Your two cards are face up to you alone; the gold outline is the seat being asked. When the ask is yours, FOLD, CHECK/CALL and RAISE are buttons under a draining clock, and a lapsed clock checks what is free and folds what is not. Up to four humans take chairs in arrival order, the house plays the rest and any stack that busts rebuys at the next deal.

## The panel

Hands, streets and showdowns; **asks against skipped against reopened**, the example's deliverable: how often a seat was actually asked, how often a rebuild walked past an all-in chair, and how often a raise moved a finish line somebody thought they had reached. Folds, all-ins, uncontested pots and lapsed clocks ride along.
