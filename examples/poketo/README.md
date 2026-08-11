# poketo

A town you walk around, and battles you drop into. **Two netcode regimes in one game**, and the point is that they are genuinely different rather than one being a cheap version of the other.

The overworld is real-time and discrete: a trainer is standing on a tile or walking to the next one, and there is no third state. Battles are turn-based and instanced, which inverts everything the rest of this tree assumes. Nothing is predicted, interpolated, quantised or budgeted; latency is irrelevant, because a turn takes as long as the slower player takes to choose. All the difficulty moves into delivery, ordering and reconnection, which is the half of multiplayer nothing else here exercises.

Nothing is borrowed from any existing creature game. The creatures are invented, three of them, because a battle needs a reason to choose rather than a collection to complete.

```sh
cargo test -p poketo --test town -- --nocapture
```

## Discreteness buys exactness, not bytes

The plan for this example predicted that a tile position would be dramatically cheaper than a continuous one, and that the saving would let the view radius grow enormously. Both halves were measured and both were wrong in the same direction.

```
one trainer, on the wire:

  continuous, full width   106 bits
  continuous, quantised     51 bits
  a tile                    36 bits
```

**2.9x against a naive wire of two floats and an angle, and 1.4x against the quantised position every other example here actually sends.** Comparing tiles against raw `f32` flatters the tile; nothing in this tree sends raw floats. Against a fair opponent the saving is modest.

What discreteness buys instead is **exactness**. A tile is an index rather than a measurement, so it has no bounds to outgrow, no quantiser, no precision to argue about, and two machines comparing positions can use `==`. cube_yard shipped a bug that cannot exist here, by widening its world past the range its quantiser covered and freezing everything that wandered out. Not needing the apparatus is the result; the byte saving is a side effect.

The same correction applies to the view radius:

```
300 trainers in a town 80 tiles across, one client's share at 60Hz:

  radius    in view       as tiles    as a position
       8       12.1         3.2 KiB/s         4.5 KiB/s
      24       61.5        16.2 KiB/s        23.0 KiB/s
      80      296.5        78.2 KiB/s       110.7 KiB/s
```

Ten times the radius is **24.5x the people**, not the 100x its area suggests, because a town runs out of people before a radius runs out of tiles. But it is nowhere near free either: the per-client cost still climbs roughly with area until it saturates. Discreteness makes each person cheaper; it does not make the radius cheap.

## A step is a rule, not a sample

A trainer occupies one tile or the next, with a four-bit phase saying how far along. **Arriving is what moves the tile**, and a facing cannot change part way through a step.

That is what lets a client draw a step entirely from its start: a known beginning, a known end and a known duration, with nothing to predict and nothing to smooth. No snapshot buffer, no interpolation clock, no error smoother. The whole rung `client_utils` exists for is replaced by arithmetic, because the *rule* is shared rather than the positions.

It also has to begin and advance on the same tick. Beginning on one and advancing from the next makes a step one tick longer than its name and leaves its first frame at phase zero, which reads as a stutter before every move.

## Two rhythms in one tick

The overworld goes out **every tick**, because a trainer nobody describes stops moving on screen. A battle goes out **only when something happens**, because nothing in it decays.

That is not an optimisation of one design into another. It is what the two regimes are: **a state has to be repeated to stay true and a transcript does not.** A client in a battle receives nothing at all on a tick, and is completely up to date however long ago its last frame arrived.

The switch between them is which collection holds a seat, never a flag on a player. A trainer in a battle is not walked, not sent the overworld, and not visible to anyone still in it. A boolean would leave a body standing in the grass while its owner is elsewhere, and every rule would have to remember to check it.

## Reconnection, which is where a turn-based game keeps its difficulty

Two decisions do all the work, and neither is a mechanism.

**A choice names the turn it is for.** A resend after a dropped connection names a turn that has already resolved, so it is stale and ignored rather than applied twice. That single field is ordering, deduplication and late-arrival handling together: no sequence number for the server to remember, no dedup table, no window to age out. The bug it prevents is invisible from both ends, which is why it is worth a test that compares the *whole battle* before and after a resend rather than just the health.

**A dropped connection parks a battle rather than ending it.** Nothing in a turn-based battle decays, so it is exactly as valid a minute later; ending it discards the only state here worth resuming. A reconnecting client is a **new connection with a new id**, so a token issued on seating is the only thing that can link it to what it was doing. The token spends once, a failed resume is silence (an expired token and a first join are the same situation from where the client is standing), and a park window is what stops it being a leak.

That combination is the whole story: the token gets you back to the battle, and the turn number makes whatever you resend on arrival harmless.

## A trade is an agreement

Neither a broadcast nor a rollback. Both sides offer, both confirm, and only then does anything change hands.

**Changing an offer clears both confirmations**, and that one line is the difference between a trade window and an exploit: without it you can agree to what you can see, then swap what you are giving before the commit lands.

Two more, both about refusing to do half a thing. An unfinished trade yields **no outcome at all** rather than one side of one, because a caller applying half a swap creates one creature and destroys another. And a committed trade refuses everything, which is what makes a resend harmless here in the same way naming a turn does in a battle.

## Where it sits

[spacemo](../spacemo/) is the far end of the same axis: nothing in its design absorbs latency, so the netcode has to. This is the near end twice over, once because movement is discrete and once because a battle is turn-based. [The netcode chapter](../../docs/guide/02-choosing-your-netcode.md) is the argument; these are the two ends of it running.
