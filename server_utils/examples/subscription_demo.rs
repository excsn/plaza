//! Subscription demo: what the second relevance channel actually costs.
//!
//! [`relevance`] answers *who is near me*. This answers *who have I chosen to
//! care about, wherever they are*, and the reason it is a separate block is
//! that neither shape expresses the other: a party as a relevance radius is an
//! infinite radius, and a grid query as a subscription is resubscribing
//! everybody every tick.
//!
//! The worry about a second channel is that it doubles the work. It does not,
//! and this is the measurement of why: the union means a subscription costs
//! only the members the distance query **missed**, so a party that stays
//! together costs nothing at all. This walks a party from standing together to
//! scattered across the world and prints what each step costs.
//!
//! ```sh
//! cargo run --example subscription_demo -p plaza_server_utils
//! ```
//!
//! [`relevance`]: plaza_server_utils::relevance

use plaza_server_utils::relevance::{GridQuantizer, SpatialGrid};
use plaza_server_utils::subscription::{Audience, Because, Subscriptions};

const WORLD: f32 = 2000.0;
const CROWD: u32 = 1200;
const VIEW_RADIUS: f32 = 200.0;
const CELL_SIZE: f32 = 128.0;
const PARTY: usize = 5;

/// A deterministic scatter, so two runs report the same numbers.
fn scattered(i: u32) -> (f32, f32) {
  let x = ((i.wrapping_mul(2_654_435_761) >> 8) % 20_000) as f32 / 20_000.0 * WORLD;
  let y = ((i.wrapping_mul(40_503) >> 4) % 20_000) as f32 / 20_000.0 * WORLD;
  (x, y)
}

fn main() {
  let quantizer = GridQuantizer::new((0.0, 0.0), CELL_SIZE);
  println!("\n  {CROWD} people in a {WORLD:.0}u world, a view of {VIEW_RADIUS:.0}u, a party of {PARTY}.\n");
  println!(
    "{:>18} {:>10} {:>12} {:>10} {:>12}",
    "party is", "near", "subscribed", "told", "added by subs"
  );

  // The viewer stands in the middle; the crowd never moves. What changes is
  // where the party is standing, which is the only variable that matters.
  let viewer: u32 = 0;
  let viewer_at = (WORLD / 2.0, WORLD / 2.0);

  for (label, together) in [
    ("all together", PARTY - 1),
    ("half apart", (PARTY - 1) / 2),
    ("all scattered", 0),
  ] {
    let mut grid = SpatialGrid::new(quantizer);
    let mut subs: Subscriptions<u32> = Subscriptions::new(PARTY - 1);

    grid.insert(viewer as u64, viewer_at.0, viewer_at.1);
    for member in 1..PARTY as u32 {
      subs.group(viewer, member);
      let at = if (member as usize) <= together {
        // Standing beside the viewer, so the distance query already has them.
        (viewer_at.0 + member as f32 * 4.0, viewer_at.1)
      } else {
        scattered(member.wrapping_mul(7919) + 100_000)
      };
      grid.insert(member as u64, at.0, at.1);
    }
    for other in PARTY as u32..CROWD {
      let at = scattered(other);
      grid.insert(other as u64, at.0, at.1);
    }

    let mut near_ids = Vec::new();
    grid.query_radius(viewer_at.0, viewer_at.1, VIEW_RADIUS, &mut near_ids);
    let near: Vec<u32> = near_ids.iter().map(|id| *id as u32).collect();

    let audience = Audience::of(&near, &subs, &viewer);
    let subscribed = audience
      .entries
      .iter()
      .filter(|(_, why)| why.is_subscribed())
      .count();

    println!(
      "{label:>18} {:>10} {:>12} {:>10} {:>13}",
      audience.near,
      subscribed,
      audience.len(),
      audience.added
    );
  }

  println!("\n  read the `told` column: it does not move. The party is covered");
  println!("  either way, and all that changes is which channel paid. What the");
  println!("  second one costs is exactly what the first one dropped, which is");
  println!("  nothing at all while the party stays together.\n");

  // What the labels are for, and the reason `Because` belongs on the wire
  // rather than staying here.
  let mut subs: Subscriptions<u32> = Subscriptions::new(4);
  subs.group(0, 1);
  let audience = Audience::of(&[0, 1, 2], &subs, &0);
  println!("  and why each entry is there, which the wire has to carry:\n");
  for (key, why) in &audience.entries {
    let what = match why {
      Because::Near => "near, and will vanish when you walk away",
      Because::Subscribed => "subscribed, and will not",
      Because::Either => "both, which is one entry rather than two",
    };
    println!("    {key}: {what}");
  }
  println!(
    "\n  a client that cannot tell those apart drops a party member the\n  moment they leave view, which is the interface the subscription\n  existed for.\n"
  );

  // The other half of the block, and the reason the reverse index exists.
  let told = subs.remove(&1);
  println!("  when seat 1 leaves, {} client(s) have to be told: {told:?}", told.len());
  println!("  found without scanning a single subscriber who did not care.\n");
}
