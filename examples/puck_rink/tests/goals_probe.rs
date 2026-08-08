use puck_rink::logic::bot_held;
use puck_rink::sim::{self, PaddleInput, World, SEATS};

#[test]
fn bots_still_score_within_a_minute() {
  let mut world = World::new();
  let mut held = [PaddleInput::default(); SEATS];
  for tick in 0u64..3600 {
    let mut applied = [PaddleInput::default(); SEATS];
    for seat in 0..SEATS {
      applied[seat] = bot_held(tick, seat, &world, &mut held[seat]);
    }
    world = sim::step(&world, &applied);
    if world.scores[0] + world.scores[1] >= 2 {
      return;
    }
  }
  panic!("no second goal in a minute of bot play: {:?}", world.scores);
}
