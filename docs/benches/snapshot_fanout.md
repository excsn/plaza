# snapshot_fanout (`session/benches/snapshot_fanout.rs`)

`cargo bench -p plaza_session --bench snapshot_fanout`

What a snapshot pass pays for addressing recipients one at a time, M4 Pro. `per_recipient` is what a per-recipient `send_snapshots` pass does: one `encode_message` and one `MessageTarget::Agent` fan-out per recipient. `uniform` is the same pass when the provider's answer does not depend on who is asking: one encode, one `MessageTarget::Agents`, and a refcounted frame per recipient. It is what `SnapshotRequest::uniform` runs, and this measurement is why that request exists.

Twelve of the eighteen shipped snapshot providers take `_target_agent` and never read it, so for those the N payloads are identical.

| payload | recipients | per_recipient | uniform | ratio |
|---|---|---|---|---|
| 256 B | 8 | 1.532 µs | 445.6 ns | 3.4x |
| 256 B | 64 | 12.395 µs | 1.877 µs | 6.6x |
| 256 B | 256 | 51.567 µs | 7.175 µs | 7.2x |
| 4 KiB | 8 | 9.781 µs | 1.417 µs | 6.9x |
| 4 KiB | 64 | 77.781 µs | 2.653 µs | 29x |
| 4 KiB | 256 | 320.79 µs | 7.755 µs | 41x |
| 40 KiB | 8 | 88.544 µs | 11.749 µs | 7.5x |
| 40 KiB | 64 | 713.51 µs | 13.348 µs | 53x |
| 40 KiB | 256 | 2.8740 ms | 19.829 µs | 145x |

**The figure that matters is 2.87ms, not the ratio.** A 60Hz tick is 16.7ms, so a pass to 256 players holding 40 KiB views spends 17% of the tick budget building 255 copies of one payload. `horde_playground` is that shape, and its provider ignores the recipient.

The `uniform` arm barely moves with payload size, 7.175 µs to 19.829 µs across a 160x range, because it is almost entirely the fan-out `benches/broadcast.rs` already priced at ~37ns per named agent. Everything above that line in the other arm is duplicate encoding.

Two predictions this refuted. That a small payload would make the duplicate encodes noise next to the fan-out: it does not, and 256 B still costs 3.4x to 7.2x. And that the gap would widen with payload rather than with recipient count: it widens with both, and more steeply with recipients.

Load was 3.09 at the start against this machine's usual 1.5, so these absolutes are not comparable with the other pages here. Both arms ran under the same conditions, so the ratios are.
