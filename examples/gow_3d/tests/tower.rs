//! Where spacemo's answer gets tested against the case it is worst at.
//!
//! spacemo asked whether a volumetric grid earns its place and answered no: a
//! flat `(x, z)` grid with a height filter on what it returns is **exact at
//! identical query cost**, because it touches the same cells and examines the
//! same candidates, and only the per-candidate test differs.
//!
//! That was measured in open space, where things are spread out. A tower is the
//! opposite arrangement and the one a height filter should struggle with: five
//! hundred people standing on eight floors that share one footprint, so a flat
//! cell holds every floor at once and the filter throws away almost all of it.
//!
//! ```sh
//! cargo test -p gow_3d --test tower -- --nocapture
//! ```

/// Metres between floors. Far enough that nobody sees through one.
const FLOOR_HEIGHT: f32 = 5.0;
/// How far a character is told about.
const VIEW: f32 = 30.0;
/// Grid cell width, a third of the view, as spacemo sizes its own.
const CELL: f32 = 10.0;

#[derive(Clone, Copy)]
struct Person {
  at: (f32, f32, f32),
}

/// A tower: `floors` crowds sharing one footprint.
fn tower(per_floor: usize, floors: usize, footprint: f32) -> Vec<Person> {
  let mut seed = 0x2545_f491_4f6c_dd1du64;
  let mut next = || {
    seed ^= seed << 13;
    seed ^= seed >> 7;
    seed ^= seed << 17;
    ((seed >> 11) as f32 / (1u64 << 53) as f32) * 2.0 - 1.0
  };
  (0..floors)
    .flat_map(|floor| {
      let y = floor as f32 * FLOOR_HEIGHT;
      (0..per_floor)
        .map(|_| Person {
          at: (next() * footprint, y, next() * footprint),
        })
        .collect::<Vec<_>>()
    })
    .collect()
}

fn near(a: (f32, f32, f32), b: (f32, f32, f32), radius: f32) -> bool {
  let (dx, dy, dz) = (b.0 - a.0, b.1 - a.1, b.2 - a.2);
  dx * dx + dy * dy + dz * dz <= radius * radius
}

/// Candidates a query pulls out of cells, and how many survive the test.
struct Work {
  examined: usize,
  returned: usize,
}

/// A flat grid on `(x, z)` with a height filter applied to what it returns.
fn flat_with_filter(people: &[Person], from: (f32, f32, f32)) -> Work {
  let reach = (VIEW / CELL).ceil() as i32;
  let (cx, cz) = ((from.0 / CELL).floor() as i32, (from.2 / CELL).floor() as i32);
  let (mut examined, mut returned) = (0, 0);
  for person in people {
    let (px, pz) = ((person.at.0 / CELL).floor() as i32, (person.at.2 / CELL).floor() as i32);
    // In one of the cells the query touches, so it comes out of the grid and
    // has to be tested. Every floor of the tower is in the same cell.
    if (px - cx).abs() <= reach && (pz - cz).abs() <= reach {
      examined += 1;
      if near(from, person.at, VIEW) {
        returned += 1;
      }
    }
  }
  Work { examined, returned }
}

/// Cells in all three axes.
fn volume(people: &[Person], from: (f32, f32, f32)) -> Work {
  let reach = (VIEW / CELL).ceil() as i32;
  let (cx, cy, cz) = (
    (from.0 / CELL).floor() as i32,
    (from.1 / CELL).floor() as i32,
    (from.2 / CELL).floor() as i32,
  );
  let (mut examined, mut returned) = (0, 0);
  for person in people {
    let (px, py, pz) = (
      (person.at.0 / CELL).floor() as i32,
      (person.at.1 / CELL).floor() as i32,
      (person.at.2 / CELL).floor() as i32,
    );
    if (px - cx).abs() <= reach && (py - cy).abs() <= reach && (pz - cz).abs() <= reach {
      examined += 1;
      if near(from, person.at, VIEW) {
        returned += 1;
      }
    }
  }
  Work { examined, returned }
}

#[test]
fn a_height_filter_is_still_exact_in_a_tower_and_still_examines_everything() {
  const PER_FLOOR: usize = 30;
  // Taller than anyone can see, which is what makes this a question at all: a
  // building whose whole height fits inside the view radius is one the volume
  // grid cannot exclude anything from either, and the first version of this
  // scene was exactly that, eight floors of a thirty metre view.
  const FLOORS: usize = 24;
  let people = tower(PER_FLOOR, FLOORS, 14.0);
  assert!(
    FLOORS as f32 * FLOOR_HEIGHT > VIEW * 3.0,
    "the tower has to out-reach the view or there is nothing to exclude"
  );

  println!("\n  {PER_FLOOR} people on each of {FLOORS} floors sharing one footprint:\n");
  println!("{:>16} {:>12} {:>12} {:>10}", "strategy", "returned", "examined", "wasted");

  let mut rows = Vec::new();
  for (name, query) in [
    ("flat + y band", flat_with_filter as fn(&[Person], (f32, f32, f32)) -> Work),
    ("volume", volume),
  ] {
    let (mut examined, mut returned) = (0usize, 0usize);
    for person in people.iter().take(120) {
      let work = query(&people, person.at);
      examined += work.examined;
      returned += work.returned;
    }
    let n = 120.0;
    let wasted = 1.0 - returned as f32 / examined.max(1) as f32;
    println!(
      "{name:>16} {:>12.1} {:>12.1} {:>9.0}%",
      returned as f32 / n,
      examined as f32 / n,
      wasted * 100.0
    );
    rows.push((name, returned as f32 / n, examined as f32 / n));
  }

  let (_, flat_returned, flat_examined) = rows[0];
  let (_, vol_returned, vol_examined) = rows[1];

  // Still exact, which is the half spacemo established and this does not
  // disturb: a height filter answers the same question a volumetric grid does.
  assert!(
    (flat_returned - vol_returned).abs() < 0.01,
    "both answer the same question: {flat_returned} against {vol_returned}"
  );

  // And this is the half a tower changes. In open space the two examined the
  // same candidates, so the filter was free. Stacked, a flat cell holds every
  // floor at once and the filter throws away most of what it pulled out.
  println!(
    "\n  the filter examines {:.1}x what the volume grid does, against roughly\n  parity in open space: a flat cell holds every floor of the tower.\n",
    flat_examined / vol_examined.max(0.01)
  );
  assert!(
    flat_examined > vol_examined * 1.5,
    "a tower is where the filter has to work for it: {flat_examined} against {vol_examined}"
  );
}

#[test]
fn one_floor_of_the_same_crowd_costs_the_filter_nothing() {
  // The control, and the reason the result above is about geometry rather than
  // about crowding: the same people on one floor put the two strategies back
  // level, exactly as spacemo measured in open space.
  let people = tower(720, 1, 14.0);
  let (mut flat, mut vol) = (0usize, 0usize);
  for person in people.iter().take(120) {
    flat += flat_with_filter(&people, person.at).examined;
    vol += volume(&people, person.at).examined;
  }
  let ratio = flat as f32 / vol.max(1) as f32;
  println!("\n  the same 720 people on one floor: filter examines {ratio:.2}x the volume grid\n");
  assert!(ratio < 1.05, "level on one floor: {ratio}");
}
