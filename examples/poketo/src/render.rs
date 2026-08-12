//! Drawing the two regimes, which look nothing like each other.
//!
//! The overworld is a tile map with a camera on it; a battle is two creatures
//! and a backdrop. Neither shares anything with the other but the texture set,
//! which is the same split the client's state has.
//!
//! **Nothing here asks the wire for anything.** The ground comes from
//! `terrain_at`, the walk frame comes from the four-bit phase that was already
//! being sent to place the trainer, and the creature comes from its kind. A
//! sprite sheet added no bytes to a frame.

use macroquad::prelude::*;

use poketo::grid::{Facing, Tile, PHASE_STEPS};
use poketo::net::client::NetClient;
use poketo::terrain::{self, Prop, Terrain};

/// Pixels one tile is drawn at.
///
/// Equal to the source tile so the map draws one texel to one pixel. A
/// non-integer ratio shimmers on pixel art at every fractional camera offset,
/// which is what 28 against a 32-pixel tile was doing.
pub const TILE: f32 = 32.0;

const SOURCE_TILE: f32 = 32.0;
const CREATURE_CELL: f32 = 96.0;

/// How long an arrival takes to resolve, in milliseconds.
const TELEPORT_MS: f32 = 650.0;

/// Every texture, loaded once.
///
/// Embedded rather than fetched: a missing asset is then a compile error on
/// every target instead of a 404 in one browser, and the bytes inherit the
/// wasm's own cache stamp, which a texture the wasm fetches for itself never
/// could.
pub struct Art {
  pub terrain: Texture2D,
  pub props: Texture2D,
  pub trainer: Texture2D,
  pub creatures: Texture2D,
  pub backdrop: Texture2D,
}

fn embedded(bytes: &[u8]) -> Texture2D {
  let texture = Texture2D::from_file_with_format(bytes, Some(ImageFormat::Png));
  texture.set_filter(FilterMode::Nearest);
  texture
}

impl Art {
  pub fn load() -> Self {
    Self {
      terrain: embedded(include_bytes!("../assets/terrain.png")),
      props: embedded(include_bytes!("../assets/props.png")),
      trainer: embedded(include_bytes!("../assets/trainer.png")),
      creatures: embedded(include_bytes!("../assets/creatures.png")),
      backdrop: embedded(include_bytes!("../assets/backdrop.png")),
    }
  }
}

/// A cell of a sheet, held half a texel inside its own edges.
///
/// Sampling a rect that sits exactly on a cell boundary picks up the
/// neighbouring cell when the destination is a different size, which draws a
/// sliver of whatever is next to it down one edge of every sprite.
fn cell(col: u32, row: u32, size: f32) -> Rect {
  const INSET: f32 = 0.5;
  Rect::new(
    col as f32 * size + INSET,
    row as f32 * size + INSET,
    size - INSET * 2.0,
    size - INSET * 2.0,
  )
}

/// Which cell of the sheet a kind of ground is drawn from.
///
/// A sheet's layout is art rather than a rule, so it lives here and not beside
/// `terrain_at`. Variants are picked per tile so a field of grass is not one
/// texture repeated in a visible lattice.
fn terrain_source(at: Tile, ground: Terrain) -> Rect {
  let vary = terrain::variant(at);
  let (col, row) = match ground {
    Terrain::Path => (vary % 2, 0),
    Terrain::Grass => match vary % 4 {
      0 => (2, 0),
      1 => (3, 0),
      2 => (0, 3),
      _ => (2, 3),
    },
    Terrain::TallGrass => (vary % 2, 1),
    // Deep or shallow by how far under the waterline it is, not at random: a
    // lake picking per tile comes out as a checkerboard rather than as water.
    Terrain::Water => (if terrain::depth(at) > 2 { 3 } else { 2 }, 1),
    Terrain::Tree => (vary % 2, 2),
    Terrain::Spring => (3, 3),
  };
  cell(col, row, SOURCE_TILE)
}

fn prop_source(prop: Prop) -> Rect {
  let col = match prop {
    Prop::Flowers => 0,
    Prop::Rock => 1,
    Prop::Sign => 2,
  };
  cell(col, 0, SOURCE_TILE)
}

/// Row is the facing, column is where the gait has got to.
///
/// The four-bit phase that exists to place a trainer between two tiles also
/// drives the animation, so the walk cycle costs nothing the wire was not
/// already paying.
///
/// The beat counts **half tiles**, from the tile and the phase together, and
/// that is what makes it a gait rather than a twitch. Deriving the frame from
/// the phase alone restarts the cycle every tile, and since a phase is zero for
/// one tick on arrival it drops a standing frame into the middle of every step,
/// which is a hitch seven times a second. Counting through the tile instead
/// means the sequence never restarts, and because a whole tile is two beats,
/// arriving always lands on an even beat: a trainer that is not walking stands
/// still, and one that is alternates its feet.
fn beat_of(at: Tile, phase: u8) -> u32 {
  (at.x + at.y) * 2 + u32::from(phase >= PHASE_STEPS / 2)
}

fn walk_source(at: Tile, facing: Facing, phase: u8) -> Rect {
  let row = match facing {
    Facing::South => 0,
    Facing::North => 1,
    Facing::East => 2,
    Facing::West => 3,
  };
  let col = [1, 0, 1, 2][(beat_of(at, phase) % 4) as usize];
  cell(col, row, SOURCE_TILE)
}

/// A pixel of rise and fall on the off beat.
///
/// The sheet's three columns are near enough the same drawing that cycling them
/// alone reads as a shimmer rather than as a step. The bob is what carries the
/// walk, and it lands on the same beat, so the two agree instead of fighting.
fn walk_bob(at: Tile, phase: u8) -> f32 {
  if phase > 0 && beat_of(at, phase).is_multiple_of(2) {
    -1.0
  } else {
    0.0
  }
}

/// The town, centred on whoever is playing.
pub fn draw_town(client: &NetClient, art: &Art) {
  let Some(mine) = client.mine() else {
    draw_text("walking into town", 24.0, 48.0, 28.0, GRAY);
    return;
  };
  let (mx, my) = mine.drawn();
  let (cx, cy) = (screen_width() / 2.0, screen_height() / 2.0);

  // The camera origin is rounded once and every tile is placed from it, rather
  // than each tile being rounded on its own. Rounding per tile puts neighbours
  // 31 or 33 pixels apart depending on where the camera happens to be, and the
  // one-pixel gaps that opens are a grid of seams across the whole map.
  let (ox, oy) = ((cx - mx * TILE).floor(), (cy - my * TILE).floor());
  let place = |tx: f32, ty: f32| ((ox + tx * TILE).floor(), (oy + ty * TILE).floor());

  let across = (screen_width() / TILE / 2.0).ceil() as i32 + 2;
  let down = (screen_height() / TILE / 2.0).ceil() as i32 + 2;
  for dy in -down..=down {
    for dx in -across..=across {
      let (x, y) = (mx.floor() as i32 + dx, my.floor() as i32 + dy);
      if x < 0 || y < 0 {
        continue;
      }
      let at = Tile::new(x as u32, y as u32);
      let (px, py) = place(x as f32, y as f32);
      let params = |source: Rect| DrawTextureParams {
        source: Some(source),
        dest_size: Some(vec2(TILE, TILE)),
        ..Default::default()
      };
      let ground = terrain::terrain_at(at);
      draw_texture_ex(&art.terrain, px, py, WHITE, params(terrain_source(at, ground)));
      if let Some(prop) = terrain::prop_at(at) {
        draw_texture_ex(&art.props, px, py, WHITE, params(prop_source(prop)));
      }
      // One tile in a region of forty-eight, and the thing a beaten player is
      // looking for. A pulse is what makes it findable across a screen rather
      // than something you walk over without noticing.
      if ground == Terrain::Spring {
        let pulse = (get_time() as f32 * 2.2).sin() * 0.5 + 0.5;
        draw_circle_lines(
          px + TILE * 0.5,
          py + TILE * 0.5,
          TILE * (0.45 + pulse * 0.35),
          2.0,
          Color::new(0.65, 0.98, 1.0, 0.55 - pulse * 0.35),
        );
      }
    }
  }

  // How far through the arrival effect, or `None` once it has played out.
  let arriving = {
    let since = client.now_ms().saturating_sub(client.jumped_at) as f32;
    (client.jumped_at > 0 && since < TELEPORT_MS).then_some(since / TELEPORT_MS)
  };

  // Drawn back to front, so somebody standing lower overlaps somebody above.
  let mut trainers: Vec<_> = client.trainers().to_vec();
  trainers.sort_by_key(|t| t.at.y);
  for trainer in trainers {
    let (tx, ty) = trainer.drawn();
    let (x, y) = place(tx, ty);
    let yours = Some(trainer.seat) == client.seat;
    let top = y - TILE * 0.25 + walk_bob(trainer.at, trainer.phase);

    // Arriving from somewhere it could not have walked from: rings out, and
    // the trainer resolves out of nothing rather than appearing between two
    // frames with no account of itself.
    let (fade, squash) = match arriving.filter(|_| yours) {
      Some(t) => (t.min(1.0), 0.25 + t * 0.75),
      None => (1.0, 1.0),
    };
    if let Some(t) = arriving.filter(|_| yours) {
      let centre = vec2(x + TILE * 0.5, y + TILE * 0.2);
      for ring in 0..3 {
        let phase = (t + ring as f32 * 0.22).min(1.0);
        draw_circle_lines(
          centre.x,
          centre.y,
          TILE * (0.2 + phase * 1.5),
          2.0,
          Color::new(0.65, 0.95, 1.0, (1.0 - phase) * 0.8),
        );
      }
      draw_circle(centre.x, centre.y, TILE * 0.6 * (1.0 - t), Color::new(0.85, 0.98, 1.0, 1.0 - t));
    }

    let height = TILE * squash;
    draw_texture_ex(&art.trainer, x, top + (TILE - height), Color::new(1.0, 1.0, 1.0, fade), DrawTextureParams {
      source: Some(walk_source(trainer.at, trainer.facing, trainer.phase)),
      dest_size: Some(vec2(TILE, height)),
      ..Default::default()
    });
    if yours {
      draw_rectangle_lines(x, y - TILE * 0.25, TILE, TILE, 2.0, Color::new(1.0, 0.85, 0.2, 0.8));
    }
  }
}

/// A battle: a backdrop, two creatures, and the numbers that decide it.
pub fn draw_battle(client: &NetClient, art: &Art) {
  let Some(state) = &client.battle else {
    return;
  };
  let battle = &state.battle;

  let scale = (screen_width() / art.backdrop.width()).max(screen_height() / art.backdrop.height());
  draw_texture_ex(&art.backdrop, 0.0, 0.0, WHITE, DrawTextureParams {
    dest_size: Some(vec2(art.backdrop.width() * scale, art.backdrop.height() * scale)),
    ..Default::default()
  });

  let (w, h) = (screen_width(), screen_height());
  let size = (h * 0.28).min(220.0);

  // How far into the reaction to the last resolved turn, or `None` once it has
  // played out. A turn that changes only numbers is a turn a player cannot see
  // happen, which is what this is for.
  const HIT_MS: f32 = 420.0;
  let since = client.now_ms().saturating_sub(client.struck_at) as f32;
  let hit = (since < HIT_MS && client.struck_at > 0).then_some(since / HIT_MS);

  for side in battle.sides.iter() {
    let yours = Some(side.seat) == client.seat;
    let took = battle
      .log
      .iter()
      .find(|l| l.seat != side.seat && !l.missed && l.damage > 0)
      .is_some();
    // Yours near and low, theirs far and high, which is where a battle screen
    // of this shape has always put them.
    let (x, y) = if yours {
      (w * 0.26 - size / 2.0, h * 0.62)
    } else {
      (w * 0.72 - size / 2.0, h * 0.24)
    };
    let scale = if yours { 1.25 } else { 1.0 };

    // Struck sides shake and flash white; the one that landed the blow does
    // not, so which of the two was hit is visible without reading a number.
    let (shake, tint) = match hit.filter(|_| took) {
      Some(t) => {
        let fade = 1.0 - t;
        (
          (t * 34.0).sin() * fade * 9.0,
          Color::new(1.0, 1.0 - fade * 0.35, 1.0 - fade * 0.35, 1.0),
        )
      }
      None => (0.0, WHITE),
    };

    draw_texture_ex(&art.creatures, x + shake, y, tint, DrawTextureParams {
      source: Some(cell(side.creature.kind as u32 % 3, 0, CREATURE_CELL)),
      dest_size: Some(vec2(size * scale, size * scale)),
      flip_x: !yours,
      ..Default::default()
    });
  }
}
