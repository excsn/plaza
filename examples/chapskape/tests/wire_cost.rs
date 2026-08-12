//! What a world costs per client, since this example claims a destination is
//! the cheapest input there is.
//!
//! Two different claims, measured separately because they are not the same
//! kind of claim.
//!
//! **Ops.** gow_3d sends a held direction thirty times a second and poketo
//! sends a step whenever one is taken. This sends a place, and a place lasts as
//! long as the walk does, so the figure worth having is ops a minute rather
//! than bytes an op.
//!
//! **Bytes.** A frame here carries a moving half that has to be repeated and a
//! still half that does not, and the whole still-world argument is that the two
//! belong on different channels. That is measurable, so it is measured, with
//! the codec the example actually uses rather than counted by hand from field
//! widths.
//!
//! ```sh
//! cargo test -p chapskape --test wire_cost -- --nocapture
//! ```

#![cfg(feature = "server")]

use chapskape::controls::{Relevance, TICKS_MS};
use chapskape::logic::SkapeLogic;
use chapskape::protocol::{PlayerId, SkapeOp, TICK_MS};
use chapskape::state::SkapeState;
use chapskape::world;
use plaza::agent::Agent;
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic};
use plaza_wire::{MsgPackCodec, WireCodec};

const DRIVER_MS: u64 = chapskape::protocol::DRIVER_MS;

fn bytes_of(ops: &[SkapeOp]) -> usize {
  MsgPackCodec.encode(&ops.to_vec()).unwrap().len()
}

fn frames_for(out: &LogicOutput<SkapeOp, PlayerId>, seat: u16) -> Vec<SkapeOp> {
  out
    .ops
    .iter()
    .flat_map(|t| t.ops.iter())
    .filter(|op| match op {
      SkapeOp::World(frame) => frame.you.as_ref().map(|you| you.seat) == Some(seat),
      _ => false,
    })
    .cloned()
    .collect()
}

async fn a_busy_world(mode: Relevance, bots: usize) -> (SkapeLogic, SkapeState) {
  let logic = SkapeLogic::new().with_bots(bots);
  let mut state = SkapeState::new();
  state.mode = mode;
  logic
    .process_input(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(1u32),
    })
    .await
    .unwrap();
  (logic, state)
}

async fn one_tick(logic: &SkapeLogic, state: &mut SkapeState) -> LogicOutput<SkapeOp, PlayerId> {
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

/// The seat the joining player got, which is after the world's own.
fn my_seat(state: &SkapeState) -> u16 {
  state.seat_of(1).expect("no seat")
}

#[tokio::test]
async fn what_a_frame_costs_in_a_lived_in_world() {
  println!("\n  one client, a world with people and things in it, 60 game ticks:\n");
  println!("{:>12} {:>10} {:>12} {:>14}", "props", "bytes/frame", "bytes/second", "of that, props");

  for mode in [Relevance::EveryTick, Relevance::OnChange] {
    let (logic, mut state) = a_busy_world(mode, 24).await;
    // Let the world get going, so what is measured is a lived-in place rather
    // than a field on the first tick.
    for _ in 0..30 {
      one_tick(&logic, &mut state).await;
    }
    let seat = my_seat(&state);

    let mut total = 0usize;
    let mut props = 0usize;
    let mut frames = 0usize;
    for _ in 0..60 {
      let out = one_tick(&logic, &mut state).await;
      for op in frames_for(&out, seat) {
        let SkapeOp::World(frame) = &op else { continue };
        // What the props actually cost is the difference the same frame makes
        // without them, not the size of a frame with everything else emptied
        // out: an envelope counted as a prop is a saving nobody made.
        let bare = SkapeOp::World(Box::new(chapskape::protocol::Frame {
          objects: Vec::new(),
          ..(**frame).clone()
        }));
        let whole = bytes_of(std::slice::from_ref(&op));
        total += whole;
        props += whole - bytes_of(std::slice::from_ref(&bare));
        frames += 1;
      }
    }
    let per_frame = total as f32 / frames.max(1) as f32;
    println!(
      "{:>12} {per_frame:>10.0} {:>12.0} {:>13.1}",
      mode.label(),
      per_frame * 1000.0 / TICK_MS as f32,
      props as f32 / frames.max(1) as f32,
    );
  }
  println!("\n  the still half is most of what a frame could have been, and\n  almost none of what it is.\n");
}

#[tokio::test]
async fn what_the_tick_length_costs() {
  // The slider, priced. Everything this example says about free round trips is
  // said at six hundred milliseconds; at fifty it is an ordinary netcode
  // problem with an ordinary netcode bill.
  println!("\n  one client, the same world, at each tick length:\n");
  println!("{:>10} {:>12} {:>14}", "tick ms", "bytes/frame", "bytes/second");

  for tick_ms in TICKS_MS {
    let (logic, mut state) = a_busy_world(Relevance::OnChange, 24).await;
    state.tick_ms = tick_ms;
    for _ in 0..20 {
      one_tick(&logic, &mut state).await;
    }
    let seat = my_seat(&state);
    let mut total = 0usize;
    let mut frames = 0usize;
    for _ in 0..40 {
      let out = one_tick(&logic, &mut state).await;
      // Counted per frame rather than per wake-up: at fifty milliseconds one
      // wake-up carries a dozen of them, and dividing by wake-ups would report
      // a frame twelve times its own size.
      for op in frames_for(&out, seat) {
        total += bytes_of(std::slice::from_ref(&op));
        frames += 1;
      }
    }
    let per_frame = total as f32 / frames.max(1) as f32;
    println!(
      "{tick_ms:>10} {per_frame:>12.0} {:>14.0}",
      per_frame * 1000.0 / tick_ms as f32
    );
  }
  println!("\n  a shorter tick makes each frame a little smaller, because less\n  happens in one, and the bill several times larger anyway.\n");
}

#[tokio::test]
async fn what_a_journey_costs_against_a_held_key() {
  // The headline, stated where it can go stale loudly.
  // A journey is as far as a player can see, because a click is something they
  // aimed at. Random point to random point across the whole map would measure
  // a walk nobody ever takes and flatter the arithmetic by a factor of four.
  let mut finder = chapskape::path::Pathfinder::new();
  let mut squares = 0usize;
  let journeys = 300;
  let reach = chapskape::zone::VIEW as i16;
  for i in 0..journeys as i16 {
    let from = world::footing_near(chapskape::protocol::Tile::new(
      (i * 11) % world::SIZE,
      (i * 7) % world::SIZE,
    ));
    let angle = i as f32 * 2.399_963_2;
    let to = world::footing_near(chapskape::protocol::Tile::new(
      from.x + (angle.cos() * reach as f32) as i16,
      from.y + (angle.sin() * reach as f32) as i16,
    ));
    squares += finder
      .route(from, chapskape::path::Goal::On(to))
      .len();
  }
  let per_journey = squares as f32 / journeys as f32;
  let seconds = per_journey * TICK_MS as f32 / 1000.0;

  let walk_to = bytes_of(&[SkapeOp::WalkTo {
    tile: chapskape::protocol::Tile::new(120, 90),
  }]);

  println!("\n  a journey is {per_journey:.0} squares, which is {seconds:.1} seconds of walking.\n");
  println!("{:>26} {:>12} {:>16}", "input", "ops/journey", "bytes/journey");
  println!("{:>26} {:>12} {:>16}", "a place (this)", 1, walk_to);
  println!(
    "{:>26} {:>12.0} {:>16.0}",
    "a held direction at 30Hz",
    seconds * 30.0,
    seconds * 30.0 * 12.0
  );
  println!(
    "{:>26} {:>12.0} {:>16.0}\n",
    "a step, as poketo sends",
    per_journey,
    per_journey * 6.0
  );

  assert!(per_journey > 12.0, "journeys are too short to make the point: {per_journey}");
  assert!(walk_to < 24, "a destination should be a handful of bytes, not {walk_to}");
}

#[tokio::test]
async fn what_the_private_stream_costs() {
  // A pack is twenty-eight squares and five totals, and it is sent when it
  // moves. A player standing in a field pays nothing for it, which is the
  // whole of why a private channel is affordable at all.
  let (logic, mut state) = a_busy_world(Relevance::OnChange, 0).await;
  let seat = my_seat(&state);

  let mut with = 0usize;
  let mut without = 0usize;
  let mut sent = 0usize;
  let mut ticks = 0usize;
  for step in 0..40 {
    if step == 10 {
      state
        .zone
        .actors
        .get_mut(&seat)
        .unwrap()
        .pack
        .add(chapskape::protocol::Item::Logs);
      state.zone.actors.get_mut(&seat).unwrap().private_moved = true;
    }
    let out = one_tick(&logic, &mut state).await;
    let frames = frames_for(&out, seat);
    if frames.is_empty() {
      continue;
    }
    ticks += 1;
    for op in &frames {
      let SkapeOp::World(frame) = op else { continue };
      let carried = frame.you.as_ref().is_some_and(|you| you.private.is_some());
      let size = bytes_of(std::slice::from_ref(op));
      if carried {
        with += size;
        sent += 1;
      } else {
        without += size;
      }
    }
  }

  let carried = with as f32 / sent.max(1) as f32;
  let quiet = without as f32 / (ticks - sent).max(1) as f32;
  println!("\n  a frame carrying the pack: {carried:.0} bytes");
  println!("  a frame not carrying it:   {quiet:.0} bytes");
  println!("  sent {sent} times in {ticks} ticks\n");
  assert!(sent <= 3, "the pack went out {sent} times without changing much");
  assert!(carried > quiet, "carrying the pack cost nothing, which cannot be right");
}
