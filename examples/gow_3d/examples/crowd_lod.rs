//! What level of detail is worth on this zone, measured in pixels.
//!
//! The crowd column of `zone_scale` is the expensive one, and level of detail
//! is the standard answer to it. The question that decides whether to reach for
//! it is not bytes or microseconds, it is **what the player sees**, so that is
//! what this measures: every scheme is priced in screen-space error at 1080p
//! and a 45 degree vertical field of view, which is the camera
//! `render::over_the_shoulder` actually builds.
//!
//! The conversion is the whole argument. Pixels per world unit at distance `d`
//! is `H / (2 d tan(fovy/2))`, which is 1304/d here. So the error a player can
//! see **shrinks with distance**, and an error budget may grow linearly with it:
//! a body 46 units away can be a whole 0.035 units out of place before anyone
//! could tell, and one 5 units away cannot be 0.004 out.
//!
//! Four schemes, one metric:
//!
//! - `exact`: what ships. 18 bits over a 1024-unit range, everywhere.
//! - `graded`: bits chosen per distance band so the step stays under a pixel.
//!   Keeps every individual; spends less on the far ones.
//! - `merged`: `AggregateTree`, the library's Barnes-Hut block, which replaces
//!   a distant cluster with its weighted centroid.
//! - `culled`: a view cap at half the radius, which is what a naive fix reaches
//!   for and is here to be beaten.
//!
//! Run with `cargo run -p gow_3d --release --example crowd_lod`.

use gow_3d::state::GowState;
use gow_3d::terrain;
use gow_3d::zone::VIEW;
use plaza_server_utils::aggregate::{AggregateTree, WeightedPoint};

/// Vertical resolution and field of view the error is judged at.
const SCREEN_H: f32 = 1080.0;
const FOVY_DEG: f32 = 45.0;

/// Population the crowd is measured at.
const CROWD: usize = 256;

/// Spacings, from the spawn spiral's own down to a mob in one view.
const SPREADS: [f32; 3] = [7.0, 2.5, 1.5];

/// The range the shipped layout quantises over, and what it spends.
const RANGE: f32 = 1024.0;
const SHIPPED_BITS: u32 = 18;

/// Pixels one world unit covers at `d` away.
fn px_per_unit(d: f32) -> f32 {
  let half = (FOVY_DEG.to_radians() / 2.0).tan();
  SCREEN_H / (2.0 * d.max(0.001) * half)
}

/// The quantisation step `bits` buys over `RANGE`.
fn step(bits: u32) -> f32 {
  RANGE / ((1u32 << bits) - 1) as f32
}

/// Bits that keep a body at `d` inside one pixel.
fn bits_for(d: f32) -> u32 {
  let allowed = 1.0 / px_per_unit(d);
  let mut bits = 1;
  while step(bits) > allowed && bits < 24 {
    bits += 1;
  }
  bits
}

fn at(seat: u16, spread: f32) -> (f32, f32, f32) {
  const GOLDEN: f32 = 2.399_963_2;
  let angle = seat as f32 * GOLDEN;
  let radius = spread * (seat as f32).sqrt();
  let (x, z) = (angle.cos() * radius, angle.sin() * radius);
  (x, terrain::ground_at(x, z), z)
}

fn zone_of(count: usize, spread: f32) -> GowState {
  let mut state = GowState::with_capacity(count);
  for seat in 0..count as u16 {
    state.zone.admit(seat, at(seat, spread));
  }
  state
}

fn distance2d(a: (f32, f32, f32), b: (f32, f32, f32)) -> f32 {
  ((a.0 - b.0).powi(2) + (a.2 - b.2).powi(2)).sqrt()
}

/// The error a scheme puts on screen, in pixels, over one viewer's audience.
#[derive(Default)]
struct Seen {
  errors_px: Vec<f32>,
  bits: u64,
  missing: usize,
}

impl Seen {
  fn worst(&self) -> f32 {
    self.errors_px.iter().copied().fold(0.0, f32::max)
  }

  fn median(&self) -> f32 {
    if self.errors_px.is_empty() {
      return 0.0;
    }
    let mut sorted = self.errors_px.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    sorted[sorted.len() / 2]
  }

  fn bytes(&self) -> f32 {
    self.bits as f32 / 8.0
  }
}

fn main() {
  println!("\ngow_3d crowd level of detail, judged in pixels at {SCREEN_H:.0}p / {FOVY_DEG:.0} degrees");
  println!("view radius is {VIEW} units, where one pixel is {:.4} units\n", 1.0 / px_per_unit(VIEW));

  println!("== what a pixel is worth, and what the shipped layout spends ==\n");
  println!("{:>10} {:>12} {:>14} {:>12} {:>12}", "distance", "px / unit", "body height px", "1 px in u", "bits needed");
  for d in [5.0f32, 10.0, 20.0, 30.0, VIEW, 96.0] {
    println!(
      "{d:>10.0} {:>12.1} {:>14.1} {:>12.4} {:>12}",
      px_per_unit(d),
      2.0 * px_per_unit(d),
      1.0 / px_per_unit(d),
      bits_for(d)
    );
  }
  println!(
    "\n  The shipped layout spends {SHIPPED_BITS} bits everywhere, a step of {:.4} units.",
    step(SHIPPED_BITS)
  );
  println!("  That is one pixel on the nearest body and {:.0}x finer than a pixel at the", (1.0 / px_per_unit(VIEW)) / step(SHIPPED_BITS));
  println!("  view edge. The over-spend is real and it is bounded by the view radius.\n");

  println!("== four schemes, at {CROWD} characters ==\n");
  println!(
    "{:>9} {:>9} {:>10} {:>11} {:>11} {:>10} {:>9}",
    "spacing", "in view", "scheme", "worst px", "median px", "B/client", "missing"
  );

  for spread in SPREADS {
    let mut state = zone_of(CROWD, spread);
    let mut scratch = Vec::new();
    let audience = state.zone.audience_for(0, &mut scratch).seats.clone();
    let me = state.zone.characters[&0].tracked.at;

    let crowd: Vec<WeightedPoint> = audience
      .iter()
      .filter_map(|s| state.zone.characters.get(s))
      .map(|c| WeightedPoint::new(c.tracked.at.0, c.tracked.at.2, 1.0))
      .collect();

    // exact: what ships, one width for everyone.
    let mut exact = Seen::default();
    // graded: a width per body, from how far away it is.
    let mut graded = Seen::default();
    for s in &audience {
      let Some(character) = state.zone.characters.get(s) else { continue };
      let d = distance2d(me, character.tracked.at).max(0.5);
      let ppu = px_per_unit(d);

      // Half a step is the worst a round trip through a quantiser costs.
      exact.errors_px.push(step(SHIPPED_BITS) * 0.5 * ppu);
      exact.bits += u64::from(SHIPPED_BITS) * 2;

      let bits = bits_for(d);
      graded.errors_px.push(step(bits) * 0.5 * ppu);
      graded.bits += u64::from(bits) * 2;
    }

    // merged: the library's aggregation tree, at the angle its own docs call
    // the best trade in the black hole's table.
    let tree = AggregateTree::build_in(&crowd, (0.0, 0.0), 2048.0, 10);
    let mut summaries = Vec::new();
    tree.summarize(me.0, me.2, 0.5, &mut summaries);
    let mut merged = Seen::default();
    for summary in &summaries {
      for member in tree.members(summary) {
        let point = crowd[*member as usize];
        let body = (point.x, 0.0, point.y);
        let d = distance2d(me, body).max(0.5);
        // A member is drawn at the summary's centre of mass, so what the player
        // sees is the distance from where they are to where the crowd is.
        let displaced = ((point.x - summary.x).powi(2) + (point.y - summary.y).powi(2)).sqrt();
        merged.errors_px.push(displaced * px_per_unit(d));
      }
      merged.bits += u64::from(SHIPPED_BITS) * 2 + 16;
    }

    // culled: half the radius, and everything past it simply is not there.
    let mut culled = Seen::default();
    for s in &audience {
      let Some(character) = state.zone.characters.get(s) else { continue };
      let d = distance2d(me, character.tracked.at).max(0.5);
      if d > VIEW * 0.5 {
        culled.missing += 1;
        continue;
      }
      culled.errors_px.push(step(SHIPPED_BITS) * 0.5 * px_per_unit(d));
      culled.bits += u64::from(SHIPPED_BITS) * 2;
    }

    let in_view = audience.len();
    for (name, seen) in [
      ("exact", &exact),
      ("graded", &graded),
      ("merged", &merged),
      ("culled", &culled),
    ] {
      println!(
        "{spread:>9.1} {in_view:>9} {name:>10} {:>11.1} {:>11.1} {:>10.0} {:>9}",
        seen.worst(),
        seen.median(),
        seen.bytes(),
        seen.missing
      );
    }
    println!();
  }

  println!("  Read the worst column first, because a player sees the worst case and");
  println!("  averages it with nothing. One pixel is the bar: under it a scheme is");
  println!("  invisible, and over it somebody is standing somewhere they are not.\n");
  println!("  `graded` keeps every body and every one of them inside a pixel, which");
  println!("  is the same picture for fewer bits. `merged` keeps every body's");
  println!("  *presence* and moves it, which is what an aggregation tree is for and");
  println!("  is the wrong tool at this radius: at {VIEW} units a body is {:.0} pixels", 2.0 * px_per_unit(VIEW));
  println!("  tall, so a centroid it does not stand on is plainly visible. `culled`");
  println!("  has no error at all on what it keeps, and deletes the rest.\n");
}
