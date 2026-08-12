//! Where spacemo's answer gets tested against the case it is worst at.
//!
//! spacemo asked whether a volumetric grid earns its place and answered no: a
//! flat `(x, z)` grid with a height filter on what it returns is **exact at
//! identical query cost**, because it touches the same cells and examines the
//! same candidates, and only the per-candidate test differs.
//!
//! That was measured in open space, where things are spread out. A tower is the
//! opposite arrangement and the one a height filter should struggle with:
//! thirty people on each of twenty-four floors sharing one footprint, so a flat
//! cell holds every floor at once and the filter throws away almost all of it.
//!
//! The floor count is load bearing rather than decorative. The first version of
//! this was eight floors against a thirty metre view, which is a building the
//! volumetric grid cannot exclude anything from either, so both arms examined
//! everyone and the comparison had no contrast. The scene now asserts it
//! out-reaches the view.
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


/// The same question asked of the running zone rather than a synthetic scene.
///
/// The scene above is a model of two strategies. This runs the real `Zone`,
/// with the grid the server queries every tick, and reads the counters it keeps
/// while doing it. It was written expecting to reproduce the 72% waste and it
/// does not, for a reason worth more than the agreement would have been.
#[cfg(feature = "server")]
mod in_the_real_zone {
  use gow_3d::state::{spawn_at, GowState, MAX_CHARACTERS};
  use gow_3d::zone::{FLOOR_HEIGHT, VIEW};

  /// Runs one query per character and returns what the server's own grid
  /// examined against what survived the distance test.
  fn one_round(stacked: bool) -> (f64, f64) {
    let mut state = GowState::new();
    for seat in 0..MAX_CHARACTERS as u16 {
      state.zone.admit(seat, spawn_at(seat));
    }
    if stacked {
      for seat in 0..MAX_CHARACTERS as u16 {
        let floor = (seat % 24) as f32;
        let at = spawn_at(seat);
        state.zone.place(seat, (at.0 * 0.12, floor * FLOOR_HEIGHT, at.2 * 0.12));
      }
    }

    state.zone.examined = 0;
    state.zone.returned = 0;
    let mut scratch = Vec::new();
    for seat in 0..MAX_CHARACTERS as u16 {
      state.zone.near(seat, &mut scratch);
    }
    let n = MAX_CHARACTERS as f64;
    (state.zone.examined as f64 / n, state.zone.returned as f64 / n)
  }

  #[test]
  fn the_index_excludes_nobody_because_the_zone_is_smaller_than_the_view() {
    let (flat_examined, flat_returned) = one_round(false);
    let (tower_examined, tower_returned) = one_round(true);

    println!("\n  the server's own grid, {MAX_CHARACTERS} characters, per query:\n");
    println!("{:>14} {:>12} {:>12} {:>10}", "arrangement", "examined", "returned", "wasted");
    for (name, examined, returned) in [
      ("one floor", flat_examined, flat_returned),
      ("a tower", tower_examined, tower_returned),
    ] {
      println!(
        "{name:>14} {examined:>12.1} {returned:>12.1} {:>9.0}%",
        (1.0 - returned / examined.max(0.01)) * 100.0
      );
    }

    // The number that explains the rest: the grid hands back every character
    // in the zone whatever the arrangement, because a query of radius VIEW
    // covers a zone this small entirely.
    assert_eq!(
      flat_examined, MAX_CHARACTERS as f64,
      "the grid returned everyone, so it partitioned nothing"
    );
    assert_eq!(tower_examined, MAX_CHARACTERS as f64);

    println!("\n  the grid excluded nobody in either arrangement, which is the");
    println!("  finding rather than a flaw: a zone {:.0}m across against a {VIEW:.0}m view", 80.0);
    println!("  is smaller than one query, so the index is a linear scan with");
    println!("  cell arithmetic on top. It earns its keep when the world is");
    println!("  bigger than the question, and this one is not yet.\n");
    println!("  The 72% above is therefore a claim about arrangement at scale,");
    println!("  not something this zone reproduces at sixty-four characters.\n");

    // And the part that is about arrangement rather than indexing: stacking
    // changes who can see whom, which is the thing the height test decides.
    assert!(
      (tower_returned - flat_returned).abs() > 1.0,
      "stacking has to change what is visible or nothing was tested: {flat_returned:.1} to {tower_returned:.1}"
    );
  }
}
