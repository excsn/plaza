//! The battlefields: four sizes of terrain, where the squads stand at deploy,
//! and the movement the ground prices.

use std::collections::{BinaryHeap, HashMap};

use crate::protocol::{
  manhattan, on_board_of, Activation, Army, Cell, Class, MapSize, PlayerId, Terrain, Unit, SQUAD,
};

/// The artisanal small board: rocks wall the second and fifth ranks into a
/// central corridor; forests post the flanks and the corridor mouths.
const SMALL_ROWS: [&[u8]; 7] = [
  b"..........",
  b"..f....f..",
  b"....##....",
  b"..f....f..",
  b"....##....",
  b"..f....f..",
  b"..........",
];

/// Columns on each edge kept clear of generated terrain, where squads deploy.
const DEPLOY_BAND: i8 = 4;

pub fn terrain_at(map: MapSize, cell: Cell) -> Terrain {
  if map == MapSize::Small {
    return match SMALL_ROWS[cell.1 as usize][cell.0 as usize] {
      b'f' => Terrain::Forest,
      b'#' => Terrain::Rock,
      _ => Terrain::Plain,
    };
  }

  // The larger fields are patterned, not authored: deterministic from the
  // coordinates alone, so every build agrees without shipping an atlas.
  let (w, _) = map.dims();
  let (x, y) = (cell.0 as i32, cell.1 as i32);
  if cell.0 < DEPLOY_BAND || cell.0 >= w - DEPLOY_BAND {
    return Terrain::Plain;
  }
  if x % 7 == 3 && y % 5 == 2 {
    return Terrain::Rock;
  }
  if (x * 3 + y * 5) % 11 == 0 {
    return Terrain::Forest;
  }
  Terrain::Plain
}

pub fn terrain_grid(map: MapSize) -> Vec<Vec<Terrain>> {
  let (w, h) = map.dims();
  (0..h).map(|y| (0..w).map(|x| terrain_at(map, (x, y))).collect()).collect()
}

/// One squad's four stances around its slot: the knight fronts at the inner
/// column, the soldier and archer file the outer one, the healer tucks
/// between them.
fn squad_cells(x_outer: i8, x_inner: i8, y0: i8) -> [(Class, Cell); SQUAD] {
  [
    (Class::Knight, (x_inner, y0 + 1)),
    (Class::Soldier, (x_outer, y0)),
    (Class::Archer, (x_outer, y0 + 2)),
    (Class::Healer, (x_outer, y0 + 1)),
  ]
}

/// Every commander's squad, deployed. Blue along the west edge, Red mirrored
/// east; squads stack in files of four rows, spilling into a second file when
/// one column of squads cannot hold the side.
pub fn deploy(map: MapSize, commanders: &[(PlayerId, Army)]) -> Vec<Unit> {
  let (w, h) = map.dims();
  let per_file = (h / 4).max(1) as usize;

  let mut units = Vec::new();
  let mut next_id: u8 = 1;
  for army in [Army::Blue, Army::Red] {
    let mine: Vec<PlayerId> = commanders.iter().filter(|(_, a)| *a == army).map(|(p, _)| *p).collect();
    for (k, owner) in mine.iter().enumerate() {
      let file = k / per_file;
      let slot = k % per_file;
      let squads_this_file = ((mine.len() - file * per_file).min(per_file)) as i8;
      let margin = (h - squads_this_file * 4) / 2;
      let y0 = margin + slot as i8 * 4;
      let (x_outer, x_inner) = match army {
        Army::Blue => (file as i8 * 2, file as i8 * 2 + 1),
        Army::Red => (w - 1 - file as i8 * 2, w - 2 - file as i8 * 2),
      };
      for (class, at) in squad_cells(x_outer, x_inner, y0) {
        units.push(Unit {
          id: next_id,
          army,
          owner: *owner,
          class,
          at,
          hp: class.stats().hp,
          activation: Activation::Fresh,
        });
        next_id += 1;
      }
    }
  }
  units
}

/// Every cell the unit can end a march on. Terrain prices each step, enemies
/// block the way, allies may be passed through but not stood on. Sorted, so
/// the wire and the tests see one order.
pub fn reachable(map: MapSize, units: &[Unit], mover: &Unit) -> Vec<Cell> {
  let (w, h) = map.dims();
  let mov = mover.class.stats().mov;
  let mut best: HashMap<Cell, u8> = HashMap::from([(mover.at, 0)]);
  // Max-heap over reversed cost: a tiny Dijkstra, because the biggest field
  // is sixteen hundred cells and the fixed-point relaxation loop was
  // quadratic in them.
  let mut frontier: BinaryHeap<(std::cmp::Reverse<u8>, Cell)> = BinaryHeap::new();
  frontier.push((std::cmp::Reverse(0), mover.at));

  while let Some((std::cmp::Reverse(spent), at)) = frontier.pop() {
    if best.get(&at).is_some_and(|&b| spent > b) {
      continue;
    }
    for step in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
      let next = (at.0 + step.0, at.1 + step.1);
      if !on_board_of(next, w, h) {
        continue;
      }
      let Some(enter) = terrain_at(map, next).cost() else {
        continue;
      };
      if units.iter().any(|u| u.at == next && u.army != mover.army) {
        continue;
      }
      let total = spent + enter;
      if total <= mov && best.get(&next).is_none_or(|&prev| total < prev) {
        best.insert(next, total);
        frontier.push((std::cmp::Reverse(total), next));
      }
    }
  }

  let mut cells: Vec<Cell> = best
    .into_keys()
    .filter(|cell| !units.iter().any(|u| u.at == *cell))
    .collect();
  cells.sort_unstable_by_key(|c| (c.1, c.0));
  cells
}

/// Enemy units a strike lands on from where the unit stands: those at the
/// unit's exact reach. Empty for the weaponless. Sorted by id.
pub fn strike_targets(units: &[Unit], striker: &Unit) -> Vec<u8> {
  if !striker.class.armed() {
    return Vec::new();
  }
  let range = striker.class.stats().range;
  let mut targets: Vec<u8> = units
    .iter()
    .filter(|u| u.army != striker.army && manhattan(striker.at, u.at) == range)
    .map(|u| u.id)
    .collect();
  targets.sort_unstable();
  targets
}

/// Wounded allies a mend reaches: the healer's counterpart to a strike list.
/// Anyone on the army qualifies, teammates' squads included; the mender's own
/// wounds are out of its own reach.
pub fn heal_targets(units: &[Unit], healer: &Unit) -> Vec<u8> {
  if healer.class != Class::Healer {
    return Vec::new();
  }
  let range = healer.class.stats().range;
  let mut targets: Vec<u8> = units
    .iter()
    .filter(|u| {
      u.army == healer.army && u.id != healer.id && u.hp < u.class.stats().hp && manhattan(healer.at, u.at) == range
    })
    .map(|u| u.id)
    .collect();
  targets.sort_unstable();
  targets
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::protocol::MAX_COMMANDERS;

  fn unit(id: u8, army: Army, class: Class, at: Cell) -> Unit {
    Unit {
      id,
      army,
      owner: id as PlayerId,
      class,
      at,
      hp: class.stats().hp,
      activation: Activation::Fresh,
    }
  }

  fn pairs(n: usize) -> Vec<(PlayerId, Army)> {
    (0..n)
      .map(|i| (i as PlayerId + 1, if i % 2 == 0 { Army::Blue } else { Army::Red }))
      .collect()
  }

  #[test]
  fn the_forest_prices_the_corridor() {
    let knight = unit(1, Army::Blue, Class::Knight, (1, 3));
    let cells = reachable(MapSize::Small, &[knight], &knight);
    assert!(cells.contains(&(3, 3)), "through the forest costs exactly the budget");
    assert!(!cells.contains(&(4, 3)), "a step further is past it");
  }

  #[test]
  fn rocks_are_not_ground() {
    let soldier = unit(1, Army::Blue, Class::Soldier, (3, 2));
    let cells = reachable(MapSize::Small, &[soldier], &soldier);
    assert!(!cells.contains(&(4, 2)));
    assert!(!cells.contains(&(5, 2)));
  }

  #[test]
  fn enemies_block_and_allies_do_not() {
    let knight = unit(1, Army::Blue, Class::Knight, (1, 3));
    let ally = unit(2, Army::Blue, Class::Soldier, (2, 3));
    let cells = reachable(MapSize::Small, &[knight, ally], &knight);
    assert!(cells.contains(&(3, 3)), "an ally is passed through");
    assert!(!cells.contains(&(2, 3)), "but not stood on");

    let enemy = unit(5, Army::Red, Class::Soldier, (2, 3));
    let cells = reachable(MapSize::Small, &[knight, enemy], &knight);
    assert!(!cells.contains(&(3, 3)), "an enemy is a wall");
  }

  #[test]
  fn an_archer_reaches_two_and_only_two() {
    let archer = unit(4, Army::Blue, Class::Archer, (4, 0));
    let near = unit(5, Army::Red, Class::Soldier, (5, 0));
    let far = unit(6, Army::Red, Class::Soldier, (6, 0));
    let targets = strike_targets(&[archer, near, far], &archer);
    assert_eq!(targets, vec![6]);
  }

  #[test]
  fn a_healer_carries_no_weapon_and_mends_the_hurt_alone() {
    let healer = unit(1, Army::Blue, Class::Healer, (4, 0));
    let mut hurt = unit(2, Army::Blue, Class::Knight, (5, 0));
    hurt.hp = 3;
    let whole = unit(3, Army::Blue, Class::Soldier, (3, 0));
    let enemy = unit(5, Army::Red, Class::Soldier, (4, 1));

    assert_eq!(strike_targets(&[healer, enemy], &healer), Vec::<u8>::new());
    let targets = heal_targets(&[healer, hurt, whole, enemy], &healer);
    assert_eq!(targets, vec![2], "the whole and the enemy are not patients");
  }

  #[test]
  fn every_size_deploys_its_full_complement_on_clear_ground() {
    for (n, map) in [(2, MapSize::Small), (4, MapSize::Medium), (8, MapSize::Large), (MAX_COMMANDERS, MapSize::Xlarge)] {
      assert_eq!(MapSize::for_commanders(n), map);
      let units = deploy(map, &pairs(n));
      assert_eq!(units.len(), n * SQUAD, "{map:?}");

      let (w, h) = map.dims();
      let mut seen = std::collections::HashSet::new();
      for u in &units {
        assert!(on_board_of(u.at, w, h), "{map:?}: {:?} off the board", u.at);
        assert_eq!(terrain_at(map, u.at), Terrain::Plain, "{map:?}: deployed into terrain");
        assert!(seen.insert(u.at), "{map:?}: two units on {:?}", u.at);
      }
      let blue = units.iter().filter(|u| u.army == Army::Blue).count();
      assert_eq!(blue, units.len() / 2, "{map:?}: the sides are even");
    }
  }

  #[test]
  fn the_field_scales_with_the_muster() {
    assert_eq!(MapSize::for_commanders(3), MapSize::Medium);
    assert_eq!(MapSize::for_commanders(5), MapSize::Large);
    assert_eq!(MapSize::for_commanders(9), MapSize::Xlarge);
    assert_eq!(MapSize::for_commanders(32), MapSize::Xlarge);
  }
}
