//! Both halves running against each other, which is where this example's one
//! risky claim can actually be checked.
//!
//! The claim: a client can expand a destination into a route before the server
//! has heard the click, and the server will then walk that exact route, so
//! there is nothing to correct. Inside the pathfinder that is a statement about
//! a pure function. Across the seam it is a statement about **two ends holding
//! the same rule**, and only running both can say whether they do.
//!
//! The other thing worth two ends is the still world, because its two modes
//! need different client code. Under `EveryTick` a frame is the whole visible
//! set and absence means a prop is back. Under `OnChange` absence means nothing
//! happened, and a prop coming back has to be said out loud or a client draws a
//! stump for the rest of the session.
//!
//! ```sh
//! cargo test -p chapskape --test two_ends -- --nocapture
//! ```

#![cfg(all(feature = "server", feature = "client", feature = "websocket"))]

use chapskape::controls::Relevance;
use chapskape::logic::SkapeLogic;
use chapskape::net::client::NetClient;
use chapskape::protocol::{PlayerId, SkapeOp, Tile};
use chapskape::state::SkapeState;
use chapskape::world::{self, Prop};
use plaza::agent::Agent;
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic};
use plaza_wire::{frame, MsgPackCodec, WireCodec};
use plaza_ws::scripted::ScriptedSocket;

const TICK_MS: u64 = chapskape::protocol::TICK_MS;
const DRIVER_MS: u64 = chapskape::protocol::DRIVER_MS;

fn deliver(socket: &ScriptedSocket, ops: &[SkapeOp]) {
  if ops.is_empty() {
    return;
  }
  let mut bytes = Vec::new();
  frame::begin(frame::Kind::Ops, &mut bytes);
  MsgPackCodec.encode_into(&ops.to_vec(), &mut bytes).unwrap();
  socket.feed_message(bytes);
}

/// Everything one seat is sent this tick.
fn ops_for(out: &LogicOutput<SkapeOp, PlayerId>, seat: u16) -> Vec<SkapeOp> {
  out
    .ops
    .iter()
    .flat_map(|t| t.ops.iter())
    .filter(|op| match op {
      SkapeOp::World(frame) => frame.you.as_ref().map(|you| you.seat) == Some(seat),
      _ => true,
    })
    .cloned()
    .collect()
}

async fn seat(logic: &SkapeLogic, state: &mut SkapeState, id: PlayerId) {
  logic
    .process_input(state, LogicInput::AgentJoined {
      agent: Agent::new_human(id),
    })
    .await
    .unwrap();
}

async fn send(
  logic: &SkapeLogic,
  state: &mut SkapeState,
  id: PlayerId,
  op: SkapeOp,
) -> LogicOutput<SkapeOp, PlayerId> {
  logic
    .process_input(state, LogicInput::AgentOps {
      source: Agent::new_human(id),
      ops: vec![op],
    })
    .await
    .unwrap()
}

/// One whole game tick, however many wake-ups that takes.
async fn tick(logic: &SkapeLogic, state: &mut SkapeState) -> LogicOutput<SkapeOp, PlayerId> {
  let mut ops = Vec::new();
  for _ in 0..(TICK_MS / DRIVER_MS) {
    let out = logic
      .process_input(state, LogicInput::TimeStep {
        delta_time: std::time::Duration::from_millis(DRIVER_MS),
      })
      .await
      .unwrap();
    ops.extend(out.ops);
  }
  LogicOutput {
    ops,
    ..Default::default()
  }
}

/// A world with nothing in it but one player, and a client attached to them.
async fn both_sides(mode: Relevance) -> (SkapeLogic, SkapeState, NetClient, ScriptedSocket) {
  let logic = SkapeLogic::new();
  let mut state = SkapeState::new();
  state.mode = mode;
  seat(&logic, &mut state, 1).await;

  let socket = ScriptedSocket::new();
  let mut client = NetClient::from_socket(Box::new(socket.clone()));
  client.poll(0);
  let tile = state.zone.actors[&0].tile;
  deliver(&socket, &[SkapeOp::Seated { seat: 0, tile }]);
  client.poll(0);
  (logic, state, client, socket)
}

fn a_prop_near(middle: Tile, want: Prop) -> Tile {
  for radius in 1..30i16 {
    for dy in -radius..=radius {
      for dx in -radius..=radius {
        let tile = Tile::new(middle.x + dx, middle.y + dy);
        if world::prop_at(tile) == Some(want) {
          return tile;
        }
      }
    }
  }
  panic!("nothing of that kind near {middle:?}");
}

#[tokio::test]
async fn the_client_walks_the_route_the_server_walks() {
  // The whole design in one test. The client draws the journey the instant the
  // click happens; the server hears about it a round trip later and expands the
  // same square with the same rule; and the client then checks every confirmed
  // square against the route it already drew. Zero divergence is not an
  // aspiration here, it is what a shared rule means.
  let (logic, mut state, mut client, socket) = both_sides(Relevance::OnChange).await;
  let from = client.route.predicted;
  let to = world::footing_near(Tile::new(from.x + 18, from.y + 11));

  client.walk_to(to);
  let drawn: Vec<Tile> = client.route.plan().copied().collect();
  assert!(drawn.len() >= 12, "one click bought only {} squares", drawn.len());
  assert_eq!(client.ops_sent, 1, "one journey, one op");

  // The op reaches the server a tick later than the client acted on it, which
  // is the phase offset the route check exists to tolerate.
  send(&logic, &mut state, 1, SkapeOp::WalkTo { tile: to }).await;
  let server: Vec<Tile> = state.zone.actors[&0].route.iter().copied().collect();
  assert_eq!(server, drawn, "the two ends expanded one click differently");

  let mut now = 0;
  for _ in 0..(drawn.len() + 3) {
    let out = tick(&logic, &mut state).await;
    now += TICK_MS;
    deliver(&socket, &ops_for(&out, 0));
    client.poll(now);
  }

  assert_eq!(state.zone.actors[&0].tile, to, "the server never finished the walk");
  assert_eq!(client.route.predicted, to, "the client never finished it either");
  assert_eq!(client.route.diverged, 0, "the route came apart");
  assert!(
    client.route.confirmations >= drawn.len() as u64,
    "only {} squares were ever confirmed of {}",
    client.route.confirmations,
    drawn.len()
  );
  println!(
    "\n  {} squares walked for one op, {} confirmed, {} diverged\n",
    drawn.len(),
    client.route.confirmations,
    client.route.diverged
  );
}

#[tokio::test]
async fn one_op_covers_a_walk_longer_than_any_round_trip() {
  // The latency claim, as a duration rather than as prose. Whatever the network
  // costs, it is spent inside a walk the player has already committed to.
  let (logic, mut state, mut client, socket) = both_sides(Relevance::OnChange).await;
  let from = client.route.predicted;
  let tree = a_prop_near(Tile::new(from.x + 14, from.y + 14), Prop::Tree);

  client.interact(world::prop_id(tree));
  let squares = client.route.plan().count();
  send(&logic, &mut state, 1, SkapeOp::Interact {
    object: world::prop_id(tree),
  })
  .await;

  let mut now = 0;
  let mut began_on = None;
  for step in 1..=(squares + 6) {
    let out = tick(&logic, &mut state).await;
    now += TICK_MS;
    deliver(&socket, &ops_for(&out, 0));
    client.poll(now);
    if began_on.is_none()
      && client
        .you
        .as_ref()
        .is_some_and(|you| you.doing == chapskape::protocol::Doing::Chopping)
    {
      began_on = Some(step);
    }
  }
  let began = began_on.expect("never started chopping");
  let walk_ms = began as u64 * TICK_MS;
  println!("\n  the walk took {walk_ms} ms before the axe moved, on one op\n");
  assert!(
    walk_ms > 3000,
    "the walk was only {walk_ms} ms, which is not longer than a bad round trip"
  );
  assert_eq!(client.ops_sent, 1);
  assert_eq!(client.route.diverged, 0);
}

#[tokio::test]
async fn a_prop_that_comes_back_is_said_out_loud_in_either_mode() {
  // The asymmetry the two modes create, checked on a real client rather than
  // asserted about the server's counters. Under one mode absence means the prop
  // is standing; under the other it means nothing happened at all.
  for mode in [Relevance::EveryTick, Relevance::OnChange] {
    let (logic, mut state, mut client, socket) = both_sides(mode).await;
    let middle = client.route.predicted;
    let tree = a_prop_near(middle, Prop::Tree);
    let id = world::prop_id(tree);

    let mut now = 0;
    let beat = async |state: &mut SkapeState, client: &mut NetClient, now: &mut u64| {
      let out = tick(&logic, state).await;
      *now += TICK_MS;
      deliver(&socket, &ops_for(&out, 0));
      client.poll(*now);
    };

    beat(&mut state, &mut client, &mut now).await;
    assert!(client.prop_standing(id), "{mode:?}: it was never standing");

    state.zone.depleted.insert(id, state.zone.tick + 4);
    beat(&mut state, &mut client, &mut now).await;
    assert!(!client.prop_standing(id), "{mode:?}: the client was not told it went");

    for _ in 0..6 {
      beat(&mut state, &mut client, &mut now).await;
    }
    assert!(client.prop_standing(id), "{mode:?}: the client was never told it came back");
  }
}

#[tokio::test]
async fn the_still_world_costs_nothing_while_nothing_happens() {
  // The measurement, taken through the client so it counts what actually
  // arrived rather than what the server thinks it sent.
  const OUT: usize = 8;
  let mut totals = Vec::new();
  for mode in [Relevance::EveryTick, Relevance::OnChange] {
    let (logic, mut state, mut client, socket) = both_sides(mode).await;
    let middle = client.route.predicted;
    // A handful out, so there is something to repeat or not repeat. Distinct
    // squares, walked once each: a ring scan that revisits its own middle
    // counts the same tree eight times and then measures nothing.
    for dy in -13i16..=13 {
      for dx in -13i16..=13 {
        if state.zone.depleted.len() >= OUT {
          break;
        }
        let tile = Tile::new(middle.x + dx, middle.y + dy);
        if world::prop_at(tile).is_some() {
          state.zone.depleted.insert(world::prop_id(tile), 10_000);
        }
      }
    }
    assert_eq!(state.zone.depleted.len(), OUT);

    let mut now = 0;
    for _ in 0..40 {
      let out = tick(&logic, &mut state).await;
      now += TICK_MS;
      deliver(&socket, &ops_for(&out, 0));
      client.poll(now);
    }
    totals.push((mode, state.object_entries, state.object_entries_repeated));
    assert_eq!(client.objects.len(), OUT, "{mode:?}: the client lost track of the stumps");
  }

  println!("\n  {OUT} props out, forty ticks, one viewer:\n");
  for (mode, sent, would_have) in &totals {
    println!("    {:<11} {sent:>4} entries sent, {would_have:>4} if every frame carried them", mode.label());
  }
  println!();
  let quiet = totals.iter().find(|(mode, _, _)| *mode == Relevance::OnChange).unwrap();
  let loud = totals.iter().find(|(mode, _, _)| *mode == Relevance::EveryTick).unwrap();
  assert!(
    quiet.1 * 10 < loud.1,
    "the quiet mode sent {} against {}, which is not an argument",
    quiet.1,
    loud.1
  );
}

#[tokio::test]
async fn a_dropped_item_reaches_its_owner_and_nobody_else() {
  // The audience decided by a rule, checked where it matters: the client that
  // may not take it is not told it exists, so there is nothing on screen to
  // click and be refused.
  let logic = SkapeLogic::new();
  let mut state = SkapeState::new();
  seat(&logic, &mut state, 1).await;
  seat(&logic, &mut state, 2).await;

  let mine = ScriptedSocket::new();
  let mut owner = NetClient::from_socket(Box::new(mine.clone()));
  let theirs = ScriptedSocket::new();
  let mut passer_by = NetClient::from_socket(Box::new(theirs.clone()));
  owner.poll(0);
  passer_by.poll(0);
  deliver(&mine, &[SkapeOp::Seated { seat: 0, tile: state.zone.actors[&0].tile }]);
  deliver(&theirs, &[SkapeOp::Seated { seat: 1, tile: state.zone.actors[&1].tile }]);
  owner.poll(0);
  passer_by.poll(0);

  state
    .zone
    .actors
    .get_mut(&0)
    .unwrap()
    .pack
    .add(chapskape::protocol::Item::Logs);
  send(&logic, &mut state, 1, SkapeOp::Drop { slot: 0 }).await;

  let out = tick(&logic, &mut state).await;
  deliver(&mine, &ops_for(&out, 0));
  deliver(&theirs, &ops_for(&out, 1));
  owner.poll(TICK_MS);
  passer_by.poll(TICK_MS);

  assert_eq!(owner.ground.len(), 1, "the owner cannot see what they dropped");
  assert!(owner.ground[0].yours);
  assert!(owner.ground[0].public_in > 0);
  assert!(
    passer_by.ground.is_empty(),
    "somebody else was told about an item they may not take"
  );

  let mut now = TICK_MS;
  for _ in 0..chapskape::zone::OWNER_TICKS {
    let out = tick(&logic, &mut state).await;
    now += TICK_MS;
    deliver(&mine, &ops_for(&out, 0));
    deliver(&theirs, &ops_for(&out, 1));
    owner.poll(now);
    passer_by.poll(now);
  }
  assert_eq!(passer_by.ground.len(), 1, "it never became everybody's");
  assert!(!owner.ground[0].yours);
}

#[tokio::test]
async fn a_pack_arrives_once_and_then_stops_arriving() {
  let (logic, mut state, mut client, socket) = both_sides(Relevance::OnChange).await;
  let mut now = 0;
  let mut beats = 0;
  for _ in 0..6 {
    let out = tick(&logic, &mut state).await;
    now += TICK_MS;
    let ops = ops_for(&out, 0);
    for op in &ops {
      if let SkapeOp::World(frame) = op
        && frame.you.as_ref().is_some_and(|you| you.private.is_some())
      {
        beats += 1;
      }
    }
    deliver(&socket, &ops);
    client.poll(now);
  }
  assert_eq!(beats, 1, "the pack was sent {beats} times without changing");
  assert_eq!(client.pack.len(), chapskape::pack::SLOTS);
}

#[tokio::test]
async fn a_chase_is_not_a_divergence() {
  // The counter only means something if it is quiet when nothing is wrong. A
  // chase is the server steering a body toward something that keeps moving,
  // which the client never claimed to have worked out, so counting it would
  // bury the one reading the panel exists for under the commonest thing in the
  // game.
  let (logic, mut state, mut client, socket) = both_sides(Relevance::OnChange).await;
  let me = client.route.predicted;
  let patch = world::footing_near(Tile::new(me.x + 6, me.y + 4));
  state.zone.admit(9, patch, chapskape::protocol::Look::Brute);
  state.zone.actors.get_mut(&9).unwrap().max_health = 4000;
  state.zone.actors.get_mut(&9).unwrap().health = 4000;

  // The client is told the brute is there before it can click on it.
  let out = tick(&logic, &mut state).await;
  deliver(&socket, &ops_for(&out, 0));
  client.poll(TICK_MS);
  assert!(client.others.contains_key(&9));

  client.attack(9);
  send(&logic, &mut state, 1, SkapeOp::Attack { seat: 9 }).await;

  let mut now = TICK_MS;
  for step in 0..40u64 {
    // The brute wanders off every few ticks, so the server re-routes and the
    // client's own route is stale by design.
    if step % 5 == 0 {
      let away = state.zone.actors[&9].tile;
      state.zone.actors.get_mut(&9).unwrap().tile =
        world::footing_near(Tile::new(away.x + 2, away.y + 1));
    }
    let out = tick(&logic, &mut state).await;
    now += TICK_MS;
    deliver(&socket, &ops_for(&out, 0));
    client.poll(now);
  }

  assert_eq!(client.route.diverged, 0, "a chase was counted as a route coming apart");
  assert_eq!(
    client.route.predicted, state.zone.actors[&0].tile,
    "the client lost the body it was drawing"
  );

  // And an ordinary walk afterwards is checked again, because the counter has
  // to come back on once the client is answering its own question.
  let from = client.route.predicted;
  let to = world::footing_near(Tile::new(from.x - 9, from.y - 6));
  client.walk_to(to);
  let drawn = client.route.plan().count();
  send(&logic, &mut state, 1, SkapeOp::WalkTo { tile: to }).await;
  let before = client.route.confirmations;
  for _ in 0..(drawn + 3) {
    let out = tick(&logic, &mut state).await;
    now += TICK_MS;
    deliver(&socket, &ops_for(&out, 0));
    client.poll(now);
  }
  assert!(client.route.confirmations > before, "the check never came back on");
  assert_eq!(client.route.diverged, 0);
  assert_eq!(client.route.predicted, to);
}

#[tokio::test]
async fn a_body_stops_walking_when_it_gets_there() {
  // Presentation derived from interpolation, and the half that is easy to leave
  // out. The two squares a body is drawn between are only rewritten when it
  // takes a step, so an arrived body holds two different ones for ever and
  // walks on the spot until the next click. The clock is what ends the walk.
  let (logic, mut state, mut client, socket) = both_sides(Relevance::OnChange).await;
  let from = client.route.predicted;
  let to = world::footing_near(Tile::new(from.x + 8, from.y + 5));

  client.walk_to(to);
  let squares = client.route.plan().count();
  send(&logic, &mut state, 1, SkapeOp::WalkTo { tile: to }).await;

  let mut now = 0;
  for _ in 0..squares {
    let out = tick(&logic, &mut state).await;
    now += TICK_MS;
    deliver(&socket, &ops_for(&out, 0));
    client.poll(now);
  }
  assert_eq!(client.route.predicted, to, "never arrived");
  assert!(client.walking(now), "the last square is still being crossed");

  // One tick later the crossing is over and so is the walk cycle, whether or
  // not another frame ever arrives.
  client.poll(now + TICK_MS);
  assert!(!client.walking(now + TICK_MS), "the walk cycle never stopped");
  assert!(!client.walking(now + TICK_MS * 20), "and it never stopped later either");

  // Everybody else is drawn from the same rule, and had the same bug.
  let out = tick(&logic, &mut state).await;
  deliver(&socket, &ops_for(&out, 0));
  client.poll(now + TICK_MS * 2);
  state.zone.admit(9, Tile::new(to.x + 2, to.y), chapskape::protocol::Look::Hen);
  for _ in 0..3 {
    let out = tick(&logic, &mut state).await;
    now += TICK_MS;
    deliver(&socket, &ops_for(&out, 0));
    client.poll(now);
  }
  let hen = client.others.get(&9).copied().expect("the hen was never described");
  assert!(
    !hen.moving(now + TICK_MS * 20, client.tick_ms),
    "a body that stopped moving still reads as walking"
  );
}

#[tokio::test]
async fn clicking_again_mid_walk_does_not_snap_the_body_back() {
  // The one that reads as a rollback and is not one. A click taken while the
  // body is already moving reaches the server a square or two later than the
  // client acted on it, so the server re-routes from where *it* is and the two
  // walk different lines to the same place. Counting that as divergence and
  // snapping to the server's square is a correction for something that was
  // never wrong, and it is what a player sees as the world undoing their input.
  let (logic, mut state, mut client, socket) = both_sides(Relevance::OnChange).await;
  let start = client.route.predicted;
  let first = world::footing_near(Tile::new(start.x + 20, start.y + 14));

  client.walk_to(first);
  send(&logic, &mut state, 1, SkapeOp::WalkTo { tile: first }).await;

  let mut now = 0;
  for _ in 0..4 {
    let out = tick(&logic, &mut state).await;
    now += TICK_MS;
    deliver(&socket, &ops_for(&out, 0));
    client.poll(now);
  }

  // Somewhere else, decided while walking, and clicked several more times the
  // way anybody actually plays.
  let mut was = client.route.predicted;
  for step in 1..=4i16 {
    let next = world::footing_near(Tile::new(start.x - step * 4, start.y + step * 3));
    client.walk_to(next);
    assert!(
      client.route.predicted.steps_to(was) <= 2,
      "the body jumped from {was:?} to {:?} on a re-click",
      client.route.predicted
    );
    was = client.route.predicted;
    send(&logic, &mut state, 1, SkapeOp::WalkTo { tile: next }).await;
    for _ in 0..3 {
      let out = tick(&logic, &mut state).await;
      now += TICK_MS;
      deliver(&socket, &ops_for(&out, 0));
      client.poll(now);
      assert!(
        client.route.predicted.steps_to(was) <= 2,
        "the body jumped from {was:?} to {:?} while walking",
        client.route.predicted
      );
      was = client.route.predicted;
    }
  }

  assert_eq!(client.route.diverged, 0, "a re-click was counted as a route coming apart");
  assert!(
    client.notices.iter().all(|notice| !notice.text.contains("different way")),
    "the player was told the world disagreed with them"
  );

  // And the two ends do agree once the walking is over, which is what makes
  // waiting to settle the right moment rather than a way of hiding a drift.
  let last = world::footing_near(Tile::new(start.x - 16, start.y + 12));
  client.walk_to(last);
  send(&logic, &mut state, 1, SkapeOp::WalkTo { tile: last }).await;
  for _ in 0..60 {
    let out = tick(&logic, &mut state).await;
    now += TICK_MS;
    deliver(&socket, &ops_for(&out, 0));
    client.poll(now);
  }
  assert_eq!(client.route.predicted, state.zone.actors[&0].tile, "they never converged");
  assert_eq!(client.route.predicted, last);
}

#[tokio::test]
async fn the_drawn_body_never_jumps_however_fast_the_clicks_come() {
  // The property the eye actually checks, at the granularity the eye checks it:
  // polled every fifty milliseconds while clicks land every hundred and fifty,
  // swinging around the compass. A click changes where the body is going and
  // never where it is, so the drawn point moves at walking speed and nothing
  // else, whatever the route, the server, or the click rate are doing.
  let (logic, mut state, mut client, socket) = both_sides(Relevance::OnChange).await;
  let start = client.route.predicted;

  let mut now = 0u64;
  let mut last = client.drawn_at(now);
  let mut worst = 0.0f32;
  for i in 0..400u64 {
    now += 50;
    client.poll(now);

    if i % 3 == 0 {
      let angle = i as f32 * 0.9;
      let to = world::footing_near(Tile::new(
        start.x + (angle.cos() * 13.0) as i16,
        start.y + (angle.sin() * 13.0) as i16,
      ));
      client.walk_to(to);
      send(&logic, &mut state, 1, SkapeOp::WalkTo { tile: to }).await;
    }
    if i % 12 == 0 {
      let out = tick(&logic, &mut state).await;
      deliver(&socket, &ops_for(&out, 0));
      client.poll(now);
    }

    let here = client.drawn_at(now);
    let step = ((here.0 - last.0).powi(2) + (here.1 - last.1).powi(2)).sqrt();
    worst = worst.max(step);
    last = here;
  }

  // Fifty milliseconds of walking is a twelfth of a square, and a run is twice
  // that. Half a square in one frame is a teleport whatever caused it.
  println!("\n  worst movement between 50ms frames: {worst:.3} squares\n");
  assert!(worst < 0.5, "the body jumped {worst} squares between frames");
  assert_eq!(client.route.diverged, 0);
}

#[tokio::test]
async fn one_bodys_experience_is_nobody_elses_news() {
  // The defect this pins had two faces. The wire one is that everybody within
  // sight paid for every body's experience. The visible one is worse: the
  // events carried no seat, so a client had no way to tell its own from anyone
  // else's and announced every passing woodcutter's level as its own.
  let logic = SkapeLogic::new();
  let mut state = SkapeState::new();
  seat(&logic, &mut state, 1).await;
  seat(&logic, &mut state, 2).await;

  let mine = ScriptedSocket::new();
  let mut worker = NetClient::from_socket(Box::new(mine.clone()));
  let theirs = ScriptedSocket::new();
  let mut bystander = NetClient::from_socket(Box::new(theirs.clone()));
  worker.poll(0);
  bystander.poll(0);
  deliver(&mine, &[SkapeOp::Seated { seat: 0, tile: state.zone.actors[&0].tile }]);
  deliver(&theirs, &[SkapeOp::Seated { seat: 1, tile: state.zone.actors[&1].tile }]);
  worker.poll(0);
  bystander.poll(0);

  // One of them chops, the other stands beside them and watches.
  let tree = a_prop_near(state.zone.actors[&0].tile, Prop::Tree);
  state.zone.actors.get_mut(&0).unwrap().tile =
    world::footing_near(Tile::new(tree.x + 1, tree.y));
  state.zone.actors.get_mut(&1).unwrap().tile =
    world::footing_near(Tile::new(tree.x + 2, tree.y + 1));
  send(&logic, &mut state, 1, SkapeOp::Interact {
    object: world::prop_id(tree),
  })
  .await;

  // Collected as they arrive rather than read at the end: a notice lives a few
  // seconds and forty ticks is most of a minute, so the end of the run has
  // forgotten everything the test is about.
  let mut worker_said: std::collections::BTreeSet<String> = Default::default();
  let mut bystander_said: std::collections::BTreeSet<String> = Default::default();

  let mut now = 0;
  for _ in 0..40 {
    let out = tick(&logic, &mut state).await;
    now += TICK_MS;
    for op in ops_for(&out, 1) {
      // The bystander's own frame must not mention any of it, in either half.
      if let SkapeOp::World(frame) = &op {
        assert!(
          frame.you.as_ref().is_none_or(|you| you.happened.is_empty()),
          "the bystander was told about somebody else's work: {:?}",
          frame.you.as_ref().map(|you| &you.happened)
        );
        assert!(
          frame.events.is_empty(),
          "somebody else's woodcutting reached the shared transcript: {:?}",
          frame.events
        );
      }
      deliver(&theirs, std::slice::from_ref(&op));
    }
    deliver(&mine, &ops_for(&out, 0));
    worker.poll(now);
    bystander.poll(now);
    worker_said.extend(worker.notices.iter().map(|notice| notice.text.clone()));
    bystander_said.extend(bystander.notices.iter().map(|notice| notice.text.clone()));
  }

  assert!(
    state.zone.actors[&0].xp[chapskape::skills::Skill::Woodcutting.index()] > 0,
    "nobody chopped anything, so the test proves nothing"
  );
  assert!(
    worker_said.iter().any(|text| text.contains("woodcutting")),
    "the one doing the work was never told about it: {worker_said:?}"
  );
  assert!(
    bystander_said.is_empty(),
    "the bystander was told: {bystander_said:?}"
  );
}
