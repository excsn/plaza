//! Drawing the horde. Reads `sim` results only; owns no state.
//!
//! Two views, and the contrast between them is the whole demonstration:
//!
//! - The **main view** follows your player and draws what your client actually
//!   received: only the enemies inside its relevance radius, drawn where the
//!   client thinks they are, over faint ground truth so the error is visible.
//! - The **minimap** shows the whole arena with every enemy the server is
//!   simulating, and a circle around what you are actually being sent. The dots
//!   outside that circle are the bandwidth relevance is saving you.

use macroquad::prelude::*;
use horde_playground::sim::{Controls, EnemyKind, Vec2 as SimVec2, World, ARENA_H, ARENA_W, NOVA_RADIUS, VIEW_RADIUS};

const C_YOU: Color = SKYBLUE;
const C_PEER: Color = Color::new(0.5, 0.8, 1.0, 0.9);
const C_KNOWN: Color = ORANGE;
const C_TRUTH: Color = Color::new(1.0, 0.6, 0.2, 0.35);
const C_VIEW: Color = Color::new(0.4, 0.9, 0.5, 0.55);
const C_GRID: Color = Color::new(1.0, 1.0, 1.0, 0.05);
const C_RUNNER: Color = Color::new(1.0, 0.45, 0.75, 1.0);
const C_BRUTE: Color = Color::new(0.85, 0.35, 0.25, 1.0);
const C_SHOT: Color = Color::new(1.0, 0.95, 0.5, 0.95);
/// A distant crowd the client knows only as a headcount.
const C_CROWD: Color = Color::new(0.55, 0.45, 0.95, 0.5);
const C_COIN: Color = Color::new(1.0, 0.85, 0.25, 1.0);

use horde_playground::sim::client::{Burst, DamagePopup};
use horde_playground::sim::PLAYER_MAX_HEALTH;

/// How long the world takes to fade in once there is one.
///
/// Networked only: the offline build owns both sides, so its world exists from
/// the first frame and there is no transient to mask.
#[cfg(all(feature = "client", feature = "websocket"))]
///
/// Short enough not to be a wait, long enough that the first frame arriving is a
/// transition rather than a pop.
const FADE_IN_SECS: f32 = 0.45;

/// Masks the join transient, which is not a cosmetic problem.
///
/// A client that renders in the past has nothing to draw until its timeline has
/// started and a frame has been played out of it. Every game that renders in the
/// past holds a screen over that gap and fades in, because the alternative is
/// showing a world that is not merely empty but *wrong*: entities at the origin,
/// then all of them arriving at once.
///
/// One overlay rather than an alpha threaded through every draw call: it masks
/// uniformly, costs one rectangle, and cannot be forgotten by whoever adds the
/// next entity type.
#[cfg(all(feature = "client", feature = "websocket"))]
pub fn draw_fade_in(ready_secs: Option<f32>) {
  let alpha = match ready_secs {
    // Nothing to show yet, so show nothing rather than something wrong.
    None => 1.0,
    Some(secs) => (1.0 - secs / FADE_IN_SECS).clamp(0.0, 1.0),
  };
  if alpha > 0.0 {
    draw_rectangle(0.0, 0.0, screen_width(), screen_height(), Color::new(0.0, 0.0, 0.0, alpha));
  }
}

/// A health bar over a player at screen `(x, y)`. Green to red as it empties, or
/// blue while the respawn shield is up.
fn draw_health_bar(x: f32, y: f32, health: u8, invuln: bool) {
  let (w, h) = (36.0, 4.0);
  let top = y - 22.0;
  let left = x - w * 0.5;
  let frac = (health as f32 / PLAYER_MAX_HEALTH).clamp(0.0, 1.0);
  draw_rectangle(left, top, w, h, Color::new(0.0, 0.0, 0.0, 0.6));
  let fill = if invuln {
    Color::new(0.5, 0.8, 1.0, 0.95)
  } else {
    Color::new((1.0 - frac) * 0.9 + 0.15, frac * 0.85 + 0.1, 0.3, 0.95)
  };
  draw_rectangle(left, top, w * frac, h, fill);
  draw_rectangle_lines(left, top, w, h, 1.0, Color::new(1.0, 1.0, 1.0, 0.25));
}

/// The respawn shield ring around a player.
fn draw_shield(x: f32, y: f32, radius: f32) {
  draw_circle_lines(x, y, radius, 2.0, Color::new(0.5, 0.85, 1.0, 0.8));
  draw_circle(x, y, radius, Color::new(0.5, 0.85, 1.0, 0.10));
}

/// Hit sparks and death explosions. A spark is a bright white flash over the
/// enemy it landed on; a death is an orange ring blowing outward.
fn draw_bursts(bursts: &[Burst], cam: &Camera) {
  for b in bursts {
    let (x, y) = cam.at(b.pos);
    let r = b.radius() * cam.scale.max(0.35);
    let a = b.alpha();
    if b.big {
      draw_circle_lines(x, y, r, 2.5, Color::new(1.0, 0.6, 0.2, a));
      draw_circle(x, y, r * 0.7, Color::new(1.0, 0.5, 0.15, a * 0.35));
    } else {
      draw_circle(x, y, r, Color::new(1.0, 1.0, 0.9, a * 0.9));
    }
  }
}

/// Floating damage numbers, rising and fading from where each shot landed.
fn draw_popups(popups: &[DamagePopup], cam: &Camera) {
  for p in popups {
    let (x, y) = cam.at(p.world_pos());
    let text = format!("{}", p.amount);
    draw_text(&text, x, y, 20.0, Color::new(1.0, 0.95, 0.55, p.alpha()));
  }
}

/// A red flash around the screen edge when you take a hit. Only the networked
/// client view has the hit-flash timer that drives it.
#[cfg(all(feature = "client", feature = "websocket"))]
fn draw_hit_vignette(age: Option<f32>) {
  let Some(age) = age else { return };
  let k = (1.0 - age / 0.35).clamp(0.0, 1.0);
  let (sw, sh) = (screen_width(), screen_height());
  let band = 64.0;
  let col = Color::new(0.9, 0.12, 0.12, 0.34 * k);
  draw_rectangle(0.0, 0.0, sw, band, col);
  draw_rectangle(0.0, sh - band, sw, band, col);
  draw_rectangle(0.0, 0.0, band, sh, col);
  draw_rectangle(sw - band, 0.0, band, sh, col);
}

/// The coin banner and the fading notice stack (upgrades, and difficulty
/// step-ups). Shared by the offline and networked views.
fn draw_notice_stack(notices: &[(String, f32)], banner: Option<&str>, sw: f32) {
  if let Some(text) = banner {
    let dims = measure_text(text, None, 22, 1.0);
    draw_text(text, sw * 0.5 - dims.width * 0.5, 34.0, 22.0, C_COIN);
  }
  for (i, (text, age)) in notices.iter().enumerate() {
    let alpha = (1.0 - (age - 2.0).max(0.0)).clamp(0.0, 1.0);
    let color = if text.contains("refused") {
      Color::new(1.0, 0.5, 0.4, alpha)
    } else if text.contains("difficulty") {
      Color::new(1.0, 0.75, 0.4, alpha)
    } else {
      Color::new(0.7, 0.95, 1.0, alpha)
    };
    let dims = measure_text(text, None, 24, 1.0);
    draw_text(text, sw * 0.5 - dims.width * 0.5, 72.0 + i as f32 * 26.0, 24.0, color);
  }
}

/// Follows one player, mapping world coordinates onto the screen.
pub struct Camera {
  center: SimVec2,
  scale: f32,
  sw: f32,
  sh: f32,
}

impl Camera {
  /// Frames the player's neighbourhood: the relevance radius fills most of the
  /// shorter screen axis, with room to see entities entering and leaving.
  pub fn follow(center: SimVec2) -> Self {
    Self::viewport(center, 1.0)
  }

  /// A free camera: any centre, any zoom. `zoom` multiplies the default scale, so
  /// 1.0 is what a player sees. This is what lets an observer roam a map far
  /// bigger than one screen.
  pub fn viewport(center: SimVec2, zoom: f32) -> Self {
    let (sw, sh) = (screen_width(), screen_height());
    let scale = (sw.min(sh) * 0.5) / (VIEW_RADIUS * 1.35) * zoom;
    Self { center, scale, sw, sh }
  }

  /// World units per screen pixel at the default zoom, for turning a mouse drag
  /// into a pan. Only the observer's free camera needs it.
  #[cfg(feature = "server")]
  pub fn base_scale() -> f32 {
    (screen_width().min(screen_height()) * 0.5) / (VIEW_RADIUS * 1.35)
  }

  fn at(&self, p: SimVec2) -> (f32, f32) {
    ((p.x - self.center.x) * self.scale + self.sw * 0.5, (p.y - self.center.y) * self.scale + self.sh * 0.5)
  }
}

pub fn draw_world(world: &World, controls: &Controls, cam: &Camera) {
  draw_coins(world, controls, cam);
  // The relevance grid, faintly, so the bucketing is visible.
  let cell = horde_playground::sim::VIEW_RADIUS * 0.61; // ~CELL_SIZE
  let start_x = ((cam.center.x - VIEW_RADIUS * 2.0) / cell).floor() * cell;
  let start_y = ((cam.center.y - VIEW_RADIUS * 2.0) / cell).floor() * cell;
  for i in 0..9 {
    let gx = start_x + i as f32 * cell;
    let gy = start_y + i as f32 * cell;
    let (x, _) = cam.at(SimVec2::new(gx, 0.0));
    let (_, y) = cam.at(SimVec2::new(0.0, gy));
    draw_line(x, 0.0, x, cam.sh, 1.0, C_GRID);
    draw_line(0.0, y, cam.sw, y, 1.0, C_GRID);
  }

  let you = world.players()[0];

  // What the server really has, faint: the error between this and the solid
  // dots is what the drawing strategy costs. On by default, because without it
  // every strategy looks equally correct.
  if controls.show_ghost {
    for (_, pos) in world.truth() {
      if pos.dist(you) <= VIEW_RADIUS * 1.3 {
        let (x, y) = cam.at(pos);
        draw_circle(x, y, 3.0, C_TRUTH);
      }
    }
  }

  // What your client actually knows and draws. Kind came with the spawn, so
  // size and colour cost nothing per update.
  for (_, pos, kind) in world.client_render(0, controls) {
    let (x, y) = cam.at(pos);
    let color = match kind {
      EnemyKind::Swarm => C_KNOWN,
      EnemyKind::Runner => C_RUNNER,
      EnemyKind::Brute => C_BRUTE,
    };
    draw_circle(x, y, kind.radius() * cam.scale.max(0.35), color);
  }

  // Shots in flight, as this client knows them.
  for pos in world.client_projectiles(0) {
    let (x, y) = cam.at(pos);
    draw_circle(x, y, 2.5, C_SHOT);
  }

  // The area pulse, while it is fresh. Without this the mass elimination it
  // causes is indistinguishable from entities silently disappearing.
  if let Some(age) = world.nova_flash_age() {
    let progress = (age / 0.45).clamp(0.0, 1.0);
    let alpha = 1.0 - progress;
    let ring = Color::new(0.6, 0.95, 1.0, alpha * 0.9);
    for player in world.players() {
      if player.dist(you) <= VIEW_RADIUS * 2.0 {
        let (px, py) = cam.at(*player);
        draw_circle_lines(px, py, NOVA_RADIUS * progress * cam.scale, 3.0, ring);
        draw_circle(px, py, NOVA_RADIUS * progress * cam.scale, Color::new(0.5, 0.9, 1.0, alpha * 0.12));
      }
    }
  }

  // The repulsor pulse, drawn from what your client believes rather than from the
  // server, so an optimistic purchase visibly fires a pulse the server is not
  // firing. The ring is the actual radius the rule is using this pulse, which
  // differs each time, so the effect explains itself rather than looking random.
  if controls.coins {
    for (i, player) in world.players().iter().enumerate() {
      if let Some(radius) = world.repulsor_pulse_for(0, i)
        && player.dist(you) <= VIEW_RADIUS * 2.0
      {
        let (px, py) = cam.at(*player);
        let r = radius * cam.scale;
        draw_circle(px, py, r, Color::new(0.55, 0.45, 0.95, 0.14));
        draw_circle_lines(px, py, r, 2.5, Color::new(0.7, 0.6, 1.0, 0.85));
        draw_circle_lines(px, py, r * 0.72, 1.5, Color::new(0.7, 0.6, 1.0, 0.4));
      }
    }
  }

  // The relevance boundary.
  let (cx, cy) = cam.at(you);
  draw_circle_lines(cx, cy, VIEW_RADIUS * cam.scale, 2.0, C_VIEW);

  // Other players, if they are near enough to see.
  for (i, p) in world.players().iter().enumerate().skip(1) {
    if p.dist(you) <= VIEW_RADIUS * 1.3 {
      let (x, y) = cam.at(*p);
      draw_circle(x, y, 7.0, C_PEER);
      let label = format!("P{i}");
      draw_text(&label, x + 9.0, y + 4.0, 16.0, C_PEER);
      if world.player_invuln(i) {
        draw_shield(x, y, 16.0);
      }
      draw_health_bar(x, y, world.player_health(i), world.player_invuln(i));
    }
  }

  draw_circle(cx, cy, 8.0, C_YOU);
  if world.player_invuln(0) {
    draw_shield(cx, cy, 16.0);
  }
  draw_health_bar(cx, cy, world.player_health(0), world.player_invuln(0));

  // Hit sparks, death explosions, and floating damage numbers from your shots.
  draw_bursts(world.client_bursts(0), cam);
  draw_popups(world.client_popups(0), cam);
}

/// The whole arena, small, showing everything the server simulates and how
/// little of it you are sent.
pub fn draw_minimap(world: &World, controls: &Controls, cam: &Camera) {
  let size = (cam.sw.min(cam.sh) * 0.26).max(140.0);
  let pad = 12.0;
  let ox = cam.sw - size - pad;
  let oy = pad;
  let s = size / ARENA_W.max(ARENA_H);

  draw_rectangle(ox, oy, size, size, Color::new(0.0, 0.0, 0.0, 0.55));
  draw_rectangle_lines(ox, oy, size, size, 1.5, DARKGRAY);

  // With crowd LOD on, this map is drawn from what *your client* holds: the
  // entities it was sent individually, plus a blob per distant crowd summary.
  // With it off the client knows nothing past its radius, so the only way to draw
  // the arena at all is to borrow the server's copy, which no real client has.
  // That is the comparison worth seeing, so the label changes with it.
  let lod = controls.crowd_lod_theta > 0.0;
  if lod {
    for (_, pos, _) in world.client_render(0, controls) {
      draw_rectangle(ox + pos.x * s, oy + pos.y * s, 1.0, 1.0, C_KNOWN);
    }
    for crowd in world.crowds(0) {
      // Area with headcount, so a big cluster reads as a big crowd.
      let r = (crowd.count as f32).sqrt() * 0.9 * s.max(0.04) * 12.0;
      draw_circle(ox + crowd.pos.x * s, oy + crowd.pos.y * s, r.clamp(1.5, size * 0.18), C_CROWD);
    }
  } else {
    for (_, pos) in world.truth() {
      draw_rectangle(ox + pos.x * s, oy + pos.y * s, 1.0, 1.0, C_TRUTH);
    }
  }
  for (i, p) in world.players().iter().enumerate() {
    let color = if i == 0 { C_YOU } else { C_PEER };
    draw_circle(ox + p.x * s, oy + p.y * s, 2.5, color);
  }
  // Your relevance radius: only what falls inside is sent to you.
  let you = world.players()[0];
  draw_circle_lines(ox + you.x * s, oy + you.y * s, VIEW_RADIUS * s, 1.5, C_VIEW);

  let caption = if lod { "whole arena (your client's own knowledge)" } else { "whole arena (server truth, borrowed)" };
  draw_text(caption, ox, oy + size + 14.0, 15.0, GRAY);
}

/// Currency on the ground, as *this client* sees it.
///
/// Drawn from the client's own list rather than the server's, so a coin the
/// client has optimistically claimed disappears immediately and reappears if the
/// server awards it to somebody else. That reappearance is the correction that
/// cannot be smoothed, and it is meant to be visible.
fn draw_coins(world: &World, controls: &Controls, cam: &Camera) {
  if !controls.coins {
    return;
  }
  for coin in world.client_coins(0) {
    let (x, y) = cam.at(coin.pos);
    draw_circle(x, y, 5.0, C_COIN);
    draw_circle_lines(x, y, 5.0, 1.0, Color::new(0.5, 0.4, 0.1, 0.9));
  }
  // Coins on their way to whoever won them. Drawn smaller and brighter so a
  // collection reads as an arrival rather than as a coin that simply vanished,
  // and so a flight cut short by a losing prediction is visible as one.
  for pos in world.coin_flights(0) {
    let (x, y) = cam.at(pos);
    draw_circle(x, y, 3.5, Color::new(1.0, 0.95, 0.6, 0.95));
  }
}

/// Announcements, and a persistent line of what you own.
///
/// Both exist because nothing else in the game says an upgrade happened. The
/// wallet changing is the only signal the protocol carries, and a number quietly
/// going down while enemy behaviour quietly changes is indistinguishable from a
/// bug, which is exactly how it read before this.
pub fn draw_notices(world: &World, controls: &Controls, cam: &Camera) {
  // The coin banner only when coins exist; the notice stack always, so a
  // difficulty step-up announces itself even with coins off.
  let banner = controls.coins.then(|| {
    let owned: Vec<&str> = world.wallet(0).upgrades.iter().map(|u| u.label()).collect();
    let (believed, _) = world.balance(0);
    if owned.is_empty() {
      format!("{believed} coins")
    } else {
      format!("{believed} coins    {}", owned.join(" + "))
    }
  });
  draw_notice_stack(world.notices(0), banner.as_deref(), cam.sw);
}

/// A small legend under the minimap.
pub fn draw_legend(cam: &Camera) {
  let items = [("you", C_YOU), ("swarm", C_KNOWN), ("runner (fast)", C_RUNNER), ("brute (tough)", C_BRUTE), ("server truth", C_TRUTH), ("shots", C_SHOT)];
  let x0 = 14.0;
  let base = cam.sh - 14.0 - (items.len() as f32 - 1.0) * 20.0;
  for (i, (label, color)) in items.iter().enumerate() {
    let y = base + i as f32 * 20.0;
    draw_circle(x0, y - 4.0, 5.0, *color);
    draw_text(label, x0 + 14.0, y, 17.0, LIGHTGRAY);
  }
}

// ---------------------------------------------------------------------------
// Networked views. Shared drawing primitives, then one entry point per role.
// ---------------------------------------------------------------------------

#[cfg(any(all(feature = "client", feature = "websocket"), feature = "server"))]
fn enemy_color(kind: EnemyKind) -> Color {
  match kind {
    EnemyKind::Swarm => C_KNOWN,
    EnemyKind::Runner => C_RUNNER,
    EnemyKind::Brute => C_BRUTE,
  }
}

/// The nova ring, drawn at every player near the eye while the pulse is fresh.
#[cfg(any(all(feature = "client", feature = "websocket"), feature = "server"))]
fn draw_nova(age: Option<f32>, players: &[SimVec2], eye: SimVec2, cam: &Camera) {
  let Some(age) = age else { return };
  let progress = (age / 0.45).clamp(0.0, 1.0);
  let alpha = 1.0 - progress;
  let ring = Color::new(0.6, 0.95, 1.0, alpha * 0.9);
  for player in players {
    if player.dist(eye) <= VIEW_RADIUS * 2.0 {
      let (px, py) = cam.at(*player);
      draw_circle_lines(px, py, NOVA_RADIUS * progress * cam.scale, 3.0, ring);
      draw_circle(px, py, NOVA_RADIUS * progress * cam.scale, Color::new(0.5, 0.9, 1.0, alpha * 0.12));
    }
  }
}

/// One player's repulsor pulse ring, if it is firing.
#[cfg(any(all(feature = "client", feature = "websocket"), feature = "server"))]
fn draw_repulsor(radius: f32, at: SimVec2, cam: &Camera) {
  let (px, py) = cam.at(at);
  let r = radius * cam.scale;
  draw_circle(px, py, r, Color::new(0.55, 0.45, 0.95, 0.14));
  draw_circle_lines(px, py, r, 2.5, Color::new(0.7, 0.6, 1.0, 0.85));
  draw_circle_lines(px, py, r * 0.72, 1.5, Color::new(0.7, 0.6, 1.0, 0.4));
}

/// Peers near the eye, and your own marker.
#[cfg(all(feature = "client", feature = "websocket"))]
fn draw_players(players: &[SimVec2], me: Option<usize>, eye: SimVec2, you: SimVec2, cam: &Camera) {
  for (i, p) in players.iter().enumerate() {
    if me == Some(i) {
      continue;
    }
    if p.dist(eye) <= VIEW_RADIUS * 1.3 {
      let (x, y) = cam.at(*p);
      draw_circle(x, y, 7.0, C_PEER);
      let label = format!("P{i}");
      draw_text(&label, x + 9.0, y + 4.0, 16.0, C_PEER);
    }
  }
  let (cx, cy) = cam.at(you);
  draw_circle_lines(cx, cy, VIEW_RADIUS * cam.scale, 2.0, C_VIEW);
  draw_circle(cx, cy, 8.0, C_YOU);
}

/// What a networked client knows and draws: its own relevant slice, its predicted
/// position, and its own ghost. No host privilege: a held packet is received
/// state.
#[cfg(all(feature = "client", feature = "websocket"))]
pub fn draw_client_world(client: &horde_playground::net::client::NetClient, controls: &Controls, cam: &Camera) {
  let you = client.my_position();
  let me = client.me.map(|m| m as usize);

  if controls.coins {
    for coin in &client.sim.coins {
      let (x, y) = cam.at(coin.pos);
      draw_circle(x, y, 5.0, C_COIN);
      draw_circle_lines(x, y, 5.0, 1.0, Color::new(0.5, 0.4, 0.1, 0.9));
    }
    for pos in client.sim.flight_positions() {
      let (x, y) = cam.at(pos);
      draw_circle(x, y, 3.5, Color::new(1.0, 0.95, 0.6, 0.95));
    }
  }

  // One instant for everything remote, obtained once. Enemies, shots and peers
  // are all drawn at `at`, so the picture cannot contradict itself: a shot leaves
  // the player who fired it, and reaches the enemy it was aimed at. Only the
  // local player is elsewhere, predicted to now, which is the one entity whose
  // input this machine already has.
  //
  // Before the first packet lands there is no instant and nothing remote to draw,
  // which is the join transient: it lasts one render delay and everything falls
  // back to the newest sample it has, so it degrades to the old behaviour rather
  // than to a blank screen.
  let at = client.sim.render_at();

  // The ghost is ahead of the markers: packets held but not yet due. Gated on the
  // server's permission too, since a client cannot draw a future it was not sent.
  let allowed = client.policy.is_none_or(|p| p.allow_ghost);
  if controls.show_ghost && allowed {
    for (pos, _) in client.sim.ghost_enemies() {
      let (x, y) = cam.at(pos);
      draw_circle(x, y, 3.0, C_TRUTH);
    }
    for (i, pos) in client.sim.players().iter().enumerate() {
      if pos.dist(you) <= VIEW_RADIUS * 1.3 {
        let (x, y) = cam.at(*pos);
        draw_circle_lines(x, y, 7.0, 1.0, C_TRUTH);
        draw_text(&format!("P{i}"), x + 9.0, y - 6.0, 14.0, C_TRUTH);
      }
    }
  }

  for (_, pos, kind) in at.map(|at| client.sim.render(controls, at)).unwrap_or_default() {
    let (x, y) = cam.at(pos);
    draw_circle(x, y, kind.radius() * cam.scale.max(0.35), enemy_color(kind));
  }
  for pos in at.map(|at| client.sim.render_projectiles(at)).unwrap_or_default() {
    let (x, y) = cam.at(pos);
    draw_circle(x, y, 2.5, C_SHOT);
  }

  let drawn = at.map(|at| client.sim.render_players(at)).unwrap_or_else(|| client.sim.players().to_vec());
  draw_nova(client.nova_flash_age(), &drawn, you, cam);
  if controls.coins {
    // No special case for your own ring any more. Every player, including you,
    // is drawn at the same render instant, so a ring pinned to its owner cannot
    // lag the marker it belongs to.
    for (i, player) in drawn.iter().enumerate() {
      if let Some(radius) = client.sim.repel_radius(i)
        && player.dist(you) <= VIEW_RADIUS * 2.0
      {
        draw_repulsor(radius, *player, cam);
      }
    }
  }

  let players = drawn;
  draw_players(&players, me, you, you, cam);

  // Hit sparks, death explosions, damage numbers, health bars, and the shield.
  draw_bursts(&client.sim.bursts, cam);
  draw_popups(&client.sim.popups, cam);
  for (i, p) in players.iter().enumerate() {
    if p.dist(you) > VIEW_RADIUS * 1.3 && me != Some(i) {
      continue;
    }
    let (x, y) = cam.at(*p);
    let health = client.sim.player_health.get(i).copied().unwrap_or(0);
    let invuln = client.sim.player_invuln.get(i).copied().unwrap_or(false);
    if invuln {
      draw_shield(x, y, 16.0);
    }
    draw_health_bar(x, y, health, invuln);
  }

  // The red flash on taking a hit, and the coin/difficulty notice stack.
  draw_hit_vignette(client.hit_flash_age());
  let banner = controls.coins.then(|| {
    let owned: Vec<&str> = client.sim.wallets.get(me.unwrap_or(0)).map(|w| w.upgrades.iter().map(|u| u.label()).collect()).unwrap_or_default();
    if owned.is_empty() {
      format!("{} coins", client.sim.believed_balance)
    } else {
      format!("{} coins    {}", client.sim.believed_balance, owned.join(" + "))
    }
  });
  draw_notice_stack(&client.sim.notices, banner.as_deref(), cam.sw);
}

/// The host's omniscient view: the client's believed slice drawn over the
/// authoritative truth, exactly as the offline playground drew it.
#[cfg(all(feature = "server", feature = "client", feature = "websocket"))]
pub fn draw_host_world(view: &horde_playground::net::arena::HostView, client: &horde_playground::net::client::NetClient, controls: &Controls, cam: &Camera) {
  let you = client.my_position();
  // The server ghost: the authoritative state under what this client believes.
  //
  // A host may legitimately draw it because it *is* the server, and it is on by
  // default because every delay here is deliberate and every one of them is
  // invisible on its own. A peer is drawn interpolated, a send interval or two in
  // the past; your own player is drawn predicted, slightly ahead; an enemy is
  // drawn wherever the chosen strategy puts it. Without something to compare
  // against, a wrong client and a correct one look identical, which is how
  // several bugs here survived for days.
  // Gated on `allow_ghost` even though a host owns the truth regardless: a host
  // that kept its ghost while denying everyone else's could not see what the
  // setting does. An observer stays omniscient; spectating is its job.
  if controls.show_ghost && controls.allow_ghost {
    for (_, pos, _) in &view.truth {
      if pos.dist(you) <= VIEW_RADIUS * 1.3 {
        let (x, y) = cam.at(*pos);
        draw_circle(x, y, 3.0, C_TRUTH);
      }
    }
    // The same for the *players*, which was missing and should not have been:
    // the only way to see how far a peer trails or how much your prediction is
    // being corrected was to infer it from where their shots came out.
    for (i, pos) in view.players.iter().enumerate() {
      if pos.dist(you) <= VIEW_RADIUS * 1.3 {
        let (x, y) = cam.at(*pos);
        draw_circle_lines(x, y, 7.0, 1.0, C_TRUTH);
        draw_text(&format!("P{i}"), x + 9.0, y - 6.0, 14.0, C_TRUTH);
      }
    }
  }
  // The host already drew the stronger ghost (truth now); the client-side one is
  // the same entities a trip earlier, and stacking both reads as noise.
  let mut inner = *controls;
  inner.show_ghost = false;
  draw_client_world(client, &inner, cam);
}

/// An observer's view: the authoritative truth, and nothing believed.
#[cfg(feature = "server")]
pub fn draw_observer_world(view: &horde_playground::net::arena::HostView, controls: &Controls, cam: &Camera) {
  use horde_playground::sim::types::repulsor_pulse;
  use horde_playground::sim::Upgrade;
  if controls.coins {
    for coin in &view.coins {
      let (x, y) = cam.at(coin.pos);
      draw_circle(x, y, 5.0, C_COIN);
      draw_circle_lines(x, y, 5.0, 1.0, Color::new(0.5, 0.4, 0.1, 0.9));
    }
  }
  for (_, pos, kind) in &view.truth {
    if pos.dist(cam.center) <= VIEW_RADIUS * 2.2 {
      let (x, y) = cam.at(*pos);
      draw_circle(x, y, kind.radius() * cam.scale.max(0.35), enemy_color(*kind));
    }
  }
  for proj in &view.projectiles {
    let (x, y) = cam.at(proj.pos);
    draw_circle(x, y, 2.5, C_SHOT);
  }
  draw_nova(view.nova_flash_age(), &view.players, cam.center, cam);
  // The repulsor from the authoritative wallets and the server clock: no belief
  // involved, so the observer sees exactly what the world is doing.
  if controls.coins {
    let pulse = repulsor_pulse(view.server_now_ms);
    for (i, player) in view.players.iter().enumerate() {
      let owns = view.wallets.get(i).is_some_and(|w| w.upgrades.contains(&Upgrade::Repulsor));
      if owns
        && let Some(radius) = pulse
      {
        draw_repulsor(radius, *player, cam);
      }
    }
  }
  for (i, p) in view.players.iter().enumerate() {
    let (x, y) = cam.at(*p);
    draw_circle(x, y, 7.0, C_PEER);
    let health = view.player_health.get(i).copied().unwrap_or(0);
    let invuln = view.player_invuln.get(i).copied().unwrap_or(false);
    if invuln {
      draw_shield(x, y, 16.0);
    }
    draw_health_bar(x, y, health, invuln);
  }
}

/// The whole arena from the *client's own* knowledge (with LOD) or borrowed truth.
#[cfg(all(feature = "client", feature = "websocket"))]
pub fn draw_client_minimap(client: &horde_playground::net::client::NetClient, controls: &Controls, cam: &Camera) {
  let (ox, oy, size, s) = minimap_frame(cam);
  let lod = controls.crowd_lod_theta > 0.0;
  if lod {
    for (_, pos, _) in client.sim.render_at().map(|at| client.sim.render(controls, at)).unwrap_or_default() {
      draw_rectangle(ox + pos.x * s, oy + pos.y * s, 1.0, 1.0, C_KNOWN);
    }
    for crowd in &client.sim.crowds {
      let r = (crowd.count as f32).sqrt() * 0.9 * s.max(0.04) * 12.0;
      draw_circle(ox + crowd.pos.x * s, oy + crowd.pos.y * s, r.clamp(1.5, size * 0.18), C_CROWD);
    }
  } else {
    // Culling alone: a real client knows nothing past its radius, so the map can
    // only show what it holds. That emptiness is the honest picture.
    for (_, pos, _) in client.sim.render_at().map(|at| client.sim.render(controls, at)).unwrap_or_default() {
      draw_rectangle(ox + pos.x * s, oy + pos.y * s, 1.0, 1.0, C_KNOWN);
    }
  }
  let you = client.my_position();
  let me = client.me.map(|m| m as usize);
  for (i, p) in client.sim.players().iter().enumerate() {
    let at = if me == Some(i) { you } else { *p };
    draw_circle(ox + at.x * s, oy + at.y * s, 2.5, if me == Some(i) { C_YOU } else { C_PEER });
  }
  draw_circle_lines(ox + you.x * s, oy + you.y * s, VIEW_RADIUS * s, 1.5, C_VIEW);
  let caption = if lod { "whole arena (your client's own knowledge)" } else { "whole arena (only what is relevant to you)" };
  draw_text(caption, ox, oy + size + 14.0, 15.0, GRAY);
}

/// The host's minimap: the same as a client's, but it may draw the server truth
/// it legitimately holds behind the client's own knowledge.
#[cfg(all(feature = "server", feature = "client", feature = "websocket"))]
pub fn draw_host_minimap(view: &horde_playground::net::arena::HostView, client: &horde_playground::net::client::NetClient, controls: &Controls, cam: &Camera) {
  let (ox, oy, size, s) = minimap_frame(cam);
  for (_, pos, _) in &view.truth {
    draw_rectangle(ox + pos.x * s, oy + pos.y * s, 1.0, 1.0, C_TRUTH);
  }
  for (_, pos, _) in client.sim.render_at().map(|at| client.sim.render(controls, at)).unwrap_or_default() {
    draw_rectangle(ox + pos.x * s, oy + pos.y * s, 1.0, 1.0, C_KNOWN);
  }
  let you = client.my_position();
  let me = client.me.map(|m| m as usize);
  for (i, p) in view.players.iter().enumerate() {
    let at = if me == Some(i) { you } else { *p };
    draw_circle(ox + at.x * s, oy + at.y * s, 2.5, if me == Some(i) { C_YOU } else { C_PEER });
  }
  draw_circle_lines(ox + you.x * s, oy + you.y * s, VIEW_RADIUS * s, 1.5, C_VIEW);
  draw_text("whole arena (server truth + your knowledge)", ox, oy + size + 14.0, 15.0, GRAY);
}

/// The observer's minimap: pure truth.
#[cfg(feature = "server")]
pub fn draw_observer_minimap(view: &horde_playground::net::arena::HostView, cam: &Camera) {
  let (ox, oy, size, s) = minimap_frame(cam);
  for (_, pos, _) in &view.truth {
    draw_rectangle(ox + pos.x * s, oy + pos.y * s, 1.0, 1.0, C_TRUTH);
  }
  for (i, p) in view.players.iter().enumerate() {
    draw_circle(ox + p.x * s, oy + p.y * s, 2.5, if i == 0 { C_YOU } else { C_PEER });
  }
  draw_text("whole arena (server truth)", ox, oy + size + 14.0, 15.0, GRAY);
}

/// The minimap box geometry: origin, size, and world-to-map scale.
#[cfg(all(feature = "client", feature = "websocket"))]
fn minimap_frame(cam: &Camera) -> (f32, f32, f32, f32) {
  let size = (cam.sw.min(cam.sh) * 0.26).max(140.0);
  let pad = 12.0;
  let ox = cam.sw - size - pad;
  let oy = pad;
  let s = size / ARENA_W.max(ARENA_H);
  draw_rectangle(ox, oy, size, size, Color::new(0.0, 0.0, 0.0, 0.55));
  draw_rectangle_lines(ox, oy, size, size, 1.5, DARKGRAY);
  (ox, oy, size, s)
}

/// The minimap box geometry for the observer (a server build may lack a client).
#[cfg(all(feature = "server", not(all(feature = "client", feature = "websocket"))))]
fn minimap_frame(cam: &Camera) -> (f32, f32, f32, f32) {
  let size = (cam.sw.min(cam.sh) * 0.26).max(140.0);
  let pad = 12.0;
  let ox = cam.sw - size - pad;
  let oy = pad;
  let s = size / ARENA_W.max(ARENA_H);
  draw_rectangle(ox, oy, size, size, Color::new(0.0, 0.0, 0.0, 0.55));
  draw_rectangle_lines(ox, oy, size, size, 1.5, DARKGRAY);
  (ox, oy, size, s)
}
