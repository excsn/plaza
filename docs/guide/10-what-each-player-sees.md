# 10. What each player sees

This chapter covers how the world reaches a client, including one that just arrived, without leaking what that client may not know.

## Snapshots as ops

Plaza's wire has one message kind for application payloads, so "here is everything" is a variant of your own op enum like any other message, rather than a protocol feature. `Snapshot(Box<WorldView>)` sits in the same enum as `Moved` and `Bid`. Your client has one decode path, your snapshot uses your own types and the transport has no special case for catching up.

Plaza decides *when* snapshots happen and *who builds them*, through the [`SnapshotProvider`](../../core/API_REFERENCE.md) seam. The controller calls your provider when a player joins and whenever your logic requests it; you return the op to send or decline.

## Hidden information

`create_snapshot` receives the target agent, because building a different payload for each recipient is the normal case. Each card player gets their own hand and everyone else's card backs; each fog-of-war commander gets the entities their units can see. Plaza keeps a secret by leaving the hidden thing out of the payload entirely instead of sending it with a flag, so no client-side code can recover it.

Two labs cover this:

- [card_table](../../examples/card_table/) deals hidden hands and its bots play from `player_view`, the same filtered payload a browser gets, because a bot reading the full table state would see every hand.
- [fog_skirmish](../../examples/fog_skirmish/) treats relevance as secrecy and counts its own leaks: a `positions_named` function lists every op variant with no wildcard arm, so any new op that names a coordinate has to be added to the leak counter. The demo includes a leak-mode button so you can see the counter move, since a counter that always reads zero might just be broken. Press it and leaks go from 0 to 28.

[pellet_maze](../../examples/pellet_maze/) found that filtering the snapshot was not enough, because the vanish leaked through an event (`Eaten` named a cell). Every message in the outbound stream has to be filtered, events included.

## The recipient's own state

Relevance decides which *other* entities a client is told about and it is easy to treat that as everything a frame carries. A client never appears in its own relevance set, so anything about the client itself is missing: your own cast bar, your cooldown, your mana, your health and the place you just respawned at.

[gow_3d](../../examples/gow_3d/) shipped that mistake and nothing on the server showed it. Its frame carried the audience and a seat number. The client drew its own body from its own position and read everything else from the list of other people, which never contained it. A player pressed the cast key, the server started, ticked and landed a bar and **nothing on screen changed at all**. Three of four keys did nothing and it looked the same as an empty zone until somebody stood next to a character and watched that character's bar run instead.

The fix is a separate block on the frame for what the player needs to know about themselves, instead of a lookup into the audience. That block also holds the other cases that are about the recipient rather than the world: how long until you may act again, how long until you are back up and the one position that travels down the wire instead of up it.

This applies outside games too. Any per-recipient view built by filtering a collection leaves out the recipient, because the filter has no reason to keep it.

## Uniform snapshots

Per-recipient building costs one provider call and one encode per recipient. When the view really is the same for everyone, `SnapshotRequest::uniform` runs the provider once with no target and encodes once, sending the same buffer to all recipients. At 256 recipients, tag_arena's uniform pass costs 19.8µs where the per-recipient pass costs 2.87ms. A uniform view must contain nothing any recipient may not see. Use per-recipient by default and uniform only when you know nothing in the view is secret, since a mistaken uniform view leaks and a needless per-recipient pass only costs microseconds.

The provider calls in a per-recipient pass are all started before any is awaited, so slow view builds for different recipients overlap instead of running one after another.

## Joining late

Because the snapshot is the catch-up mechanism, a late joiner is just a recipient who has not had a snapshot yet. State-sync games get this for free: the next frame fully describes the world. Op-stream games request a snapshot for the joiner from `AgentJoined` and let narration resume from there, which is what card_table does between deals. You also need to decide who else hears about the join, which [chapter 12](12-players-come-and-go.md) covers.

## Replacing it

The provider is one async trait with one method; `NoSnapshots` is for apps with no catch-up concept and `Ok(None)` declines a single recipient. If your snapshot needs context (a reason, a phase, a checkpoint id), `SnapshotContext` carries it from your logic to your provider without plaza reading it. If you want a different replication scheme (deltas, interest tiers, derived state), the next chapter builds it on top of this one and the snapshot stays as the resync that everything else falls back to.

## The lab

Run [card_table](../../examples/card_table/) (`cargo run -p plaza_example_card_table --bin serve`, three browser tabs) and watch each tab show a different, correct view of the same table. Then run [fog_skirmish](../../examples/fog_skirmish/), open the leak counter and try to make it move without the leak-mode button.
