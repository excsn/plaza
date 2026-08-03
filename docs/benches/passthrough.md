# passthrough (`session/benches/passthrough.rs`)

`cargo bench -p plaza_session --bench passthrough`

What one frame pays to ask whether its link is impaired, M4 Pro. Every frame asks it, in both directions, on every connection.

`lock` is the shape that was there: the 80-byte `LinkProfile` behind a `parking_lot::RwLock`, read whole to answer one question. `atomic` is the shape that is: an `AtomicBool` beside it, with the profile read only when the answer is yes.

| | passthrough | impaired |
|---|---|---|
| lock | 3.4644 ns | 3.3085 ns |
| atomic | 472.67 ps - 475.25 ps - 478.00 ps | 3.3687 ns |

**7.3x on the passthrough path**, which is what production runs: a profile is a development tool and most connections never have one. The impaired arm pays 1.8% for a load that saves it nothing, which is the whole cost of the trade.

The absolute figures matter as much as the ratio. Three nanoseconds per frame per direction is, at 4096 connections and 60Hz, about 1.5ms of CPU per second: 0.15% of one core. The lock was never expensive; it was unnecessary, and an uncontended `parking_lot` read is far cheaper than the 10-20ns it is usually assumed to cost.
