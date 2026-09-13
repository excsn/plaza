# check_raise

Fixed-limit poker at four chairs in the listen-server shape (native window, wasm page, headless server, the house filling empty seats), built for the **betting round**, a turn order the players keep reopening. It is the only turn structure in this workspace that ends when nobody owes an answer rather than when every seat has had its turn.

## The betting round

A street's asks live in a queue. A raise **rebuilds it** so that everyone active except the raiser owes an answer again. The end of the round moves with each raise and the round ends only when the queue drains: action has returned to the last aggressor and nobody owes. Folding removes a seat mid-queue. No shipped turn manager can represent an **all-in seat**: it is present, invested, eligible for everything it covered and never asked again. Each rebuild counts the seats it skipped. Preflop, the big blind is the street's opening bet and keeps its option when everyone only calls. The button rotates each hand, so each pass of the order starts at a different seat and the pass plans arrive as data.

Fixed-limit keeps the arithmetic simple: bets of 2 on the early streets and 4 on the late ones, a cap of four bets per street, blinds 1 and 2. Side pots settle in layers over each seat's total contribution, with folded money staying in. The settlement (`logic::settle`) is a pure function whose tests check that every chip is conserved: a short all-in wins only what it covered, layers above go to the best hand still eligible and odd chips land nearest the button. Hole cards are only in their own seat's view until a showdown turns the live hands face up, so card_table's per-recipient seam carries a game that depends entirely on hidden cards.

## Playing it

```sh
./run-native.sh                          # host: window plus server; prints the join address
./run-native.sh --role client --connect ws://<host>:8306/ws
./run-native.sh --role headless
./wasm-serve.sh                          # browser client on the same port
cargo run -p check_raise --bin scripted  # no window: the reopening ledger as a gate
```

Your two cards are face up to you alone; the gold outline is the seat being asked. When the ask is yours, FOLD, CHECK/CALL and RAISE are buttons under a draining clock and a lapsed clock checks what is free and folds what is not. Up to four humans take chairs in arrival order, the house plays the rest and any stack that busts rebuys at the next deal.

## The panel

Hands, streets and showdowns, then **asks, skipped and reopened**, which are the numbers this example exists to produce: how often a seat was actually asked, how often a rebuild skipped an all-in chair and how often a raise reopened a round a seat had already answered. The panel also counts folds, all-ins, uncontested pots and lapsed clocks.
