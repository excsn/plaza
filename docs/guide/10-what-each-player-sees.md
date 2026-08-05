# 10. What each player sees

The question this chapter answers: how does the world reach a client, including a client that just arrived, without leaking what they may not know?

## A snapshot is just an op

Plaza's wire has one message kind for application payloads, so "here is everything" is not a protocol feature: it is a variant of your own op enum, like any other message. `Snapshot(Box<WorldView>)` sits in the same enum as `Moved` and `Bid`. That sounds like a small economy and turns out to be a load-bearing one: your client has one decode path, your snapshot can carry exactly your vocabulary, and nothing about catching up is special-cased in the transport.

What plaza does own is *when* snapshots happen and *who builds them*: the [`SnapshotProvider`](../../core/API_REFERENCE.md) seam. The controller calls your provider when a player joins and whenever your logic requests it; you return the op to send, or decline.

## The provider is the seam for hidden information

`create_snapshot` receives the target agent. Read that again, because it is the design: **building a different payload for each recipient is the normal path, not a special case.** Each card player gets their own hand and everyone else's card backs; each fog-of-war commander gets the entities their units can see. Secrecy in plaza is not an access flag on data you sent anyway. It is *absence*: the hidden thing never enters the payload at all, so no client-side cleverness can recover what was never received.

Two labs carry this argument at different depths:

- [card_table](../../examples/card_table/) deals hidden hands, and its bots deliberately play from `player_view`, the same filtered payload a browser gets, because a bot reading the full table state would hold every hand and demonstrate the opposite of the claim.
- [fog_skirmish](../../examples/fog_skirmish/) treats relevance as secrecy and then audits itself: a `positions_named` function enumerates every op variant with no wildcard arm, so any new op that names a coordinate must declare itself to the leak counter, and the demo includes a leak-mode button because a counter that reads zero proves nothing until you can make it move. Press it and leaks go from 0 to 28.

And one hard-won warning from [pellet_maze](../../examples/pellet_maze/): filtering the snapshot was not enough, because the vanish leaked through an event (`Eaten` named a cell). Secrecy is a property of the whole outbound stream, not of one message in it. If your snapshots are filtered and your events are not, you have a well-organized leak.

## The uniform escape hatch

Per-recipient building costs one provider call and one encode per recipient. When the view genuinely is the same for everyone, `SnapshotRequest::uniform` runs the provider once with no target and encodes once, fanning the same buffer out to all recipients. The measured difference is not subtle: at 256 recipients, tag_arena's uniform pass costs 19.8µs where the per-recipient pass costs 2.87ms. The contract is printed on the tin: a uniform view must contain nothing any recipient may not see. Choose per-recipient by default and uniform when secrecy is provably absent, not the other way around; the expensive direction of that mistake is a leak, and the cheap direction is microseconds.

The provider calls in a per-recipient pass are all started before any is awaited, so a slow view build overlaps rather than serializes across recipients.

## Joining late is not a special day

Because the snapshot is the catch-up mechanism, a late joiner is just a recipient whose "last snapshot" is never. State-sync games get this free: the next frame fully describes the world. Op-stream games request a snapshot for the joiner from `AgentJoined` and let narration resume from there, which is exactly what card_table does between deals. The one subtlety worth planning for is who else needs to hear about the join, and that is [chapter 12](12-players-come-and-go.md)'s subject.

## Ripping it apart

The provider is one async trait with one method; `NoSnapshots` exists for apps with no catch-up concept, and `Ok(None)` declines a single recipient. If your snapshot needs context (a reason, a phase, a checkpoint id), `SnapshotContext` carries it from your logic to your provider without plaza reading it. If you want an entirely different replication scheme, deltas, interest tiers, derived state, that is not a replacement for this chapter but the next one stacked on top of it: the snapshot remains the resync floor that everything else falls back to.

## The lab

Deal yourself into [card_table](../../examples/card_table/) (`cargo run -p plaza_example_card_table --bin serve`, three browser tabs) and watch three tabs disagree about the same table, each correctly. Then run [fog_skirmish](../../examples/fog_skirmish/), open the leak counter, and try to make it move without the leak-mode button.
