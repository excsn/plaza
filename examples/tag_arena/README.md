# tag_arena

The state-sync netcode model: input up, world down. Playable in a browser.

```sh
cargo run -p plaza_example_tag_arena
```

Then open http://127.0.0.1:8080. Three bots are already playing, so one tab is a game; open a second tab to be chased by a person instead. WASD or the arrow keys steer. You are always moving, so steering picks a direction rather than starting and stopping.

## The model

Every other example's server narrates events: ops for what happened, snapshots only for catch-up. This one never narrates. A client sends a steer direction and the server sends back exactly one thing, every tick: the whole world. There is no op history to replay, no delta to apply in order, and no join special case. The latest snapshot IS the game, so a stale frame is discarded rather than merged, losing one costs nothing, and a mid-game joiner is caught up by the very next frame.

What makes that affordable is `SnapshotRequest::uniform`. A per-recipient pass builds and encodes the payload once per recipient, which is the right price when each player sees something different (`card_table` is that case, and is this example's opposite). Here the world holds no hidden information, so the tick handler asks for one shared pass:

```rust,ignore
let everyone = state.runners.values().map(|r| r.agent.clone()).collect();
Ok(LogicOutput::none().and_snapshot(SnapshotRequest::uniform(everyone)))
```

The provider runs once with `target_agent: None`, the payload encodes once, and `MessageTarget::Agents` hands each recipient the same refcounted frame. `docs/benches/snapshot_fanout.md` prices the difference: at 40 KiB and 256 recipients, 19.8 µs against the per-recipient pass's 2.87 ms.

The contract that buys is in the provider: the `None` view goes to everyone in the request, so it must contain nothing any recipient may not see. `WorldSnapshotProvider` ignores its target entirely, which is why the same provider also serves the per-recipient join pass unchanged.

Identity is the one thing that is not world state, and it does not travel in the snapshot. A joining client is told which runner is theirs once, as an ordinary targeted op (`ArenaOp::Welcome`), and everything after that is the world everyone shares.

## The bots read what you read

`bots.rs` runs in the server process and could read `ArenaState` directly. It reads `world_view` through `query_with` instead: the same `WorldSnapshot` a browser receives. A bot playing from privileged state is not playing the game it appears to be playing, and its behaviour stops being evidence that a real client could do the same.

They are seated with `ControllerCommand::HandleAgentJoined` and act with `SubmitAgentOps`, the push-style alternative to a session's own presence and op streams. Nothing about them is a special case inside the logic.

## Standing still puts you out of play

A runner who has not moved for two seconds is neither taggable nor eligible to be "it", and the tick hands the role to whoever is actually moving. This is one rule covering three ways a demo dies: a tab nobody has touched, a runner pinned against a wall, and a client that stopped answering. Without it, an idle tab that became "it" froze the game permanently for everyone, and the bots fleeing it converged on a single corner and stayed there. It is not a hypothetical: two idle tabs turned up uninvited while this was being tested.

Everyone can see who is out of play (`in_play` in the snapshot), because who is worth chasing is not a secret. The browser draws them faded.
