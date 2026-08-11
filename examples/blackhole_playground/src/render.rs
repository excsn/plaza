//! Drawing the field. Reads `sim` results only; owns no state.
//!
//! The camera follows your black hole. Pellets are drawn where *your client*
//! thinks they are, over the server's truth in faint grey, so any divergence
//! between the locally integrated field and the authority is visible directly.

use std::collections::HashMap;

use blackhole_playground::sim::{BlackHole, Controls, Vec2 as SimVec2, World, ARENA_H, ARENA_W, VIEW_RADIUS};
// Only the networked draws name a player id (to ask the client whether that hole
// is dashing); the offline and observer draws index holes by position.
#[cfg(all(feature = "client", feature = "websocket"))]
use blackhole_playground::sim::PlayerId;
use macroquad::prelude::*;

const C_YOU: Color = Color::new(0.55, 0.8, 1.0, 1.0);
const C_RIVAL: Color = Color::new(1.0, 0.55, 0.45, 1.0);
const C_PELLET: Color = Color::new(1.0, 0.93, 0.7, 0.95);
const C_TRUTH: Color = Color::new(0.6, 0.7, 0.9, 0.35);
const C_DISK: Color = Color::new(0.5, 0.4, 0.9, 0.30);

/// How long the dash burst is drawn for, in seconds.
///
/// A dash lasts 220 ms but the server only reports it a couple of times at the
/// send rate, so the flash is held a little longer and eased out, which turns a
/// coarse two-packet signal into something that reads as one clean burst.
const DASH_FLASH_SECS: f32 = 0.34;

/// Per-hole dash-burst timers.
///
/// The dash is a fact the server states outright (see `Packet::dashing`), so the
/// effect is driven off that rather than guessed from motion: a velocity guess
/// cannot see your own dash at all, because the client deliberately does not
/// predict the dash movement, so your drawn hole never actually speeds up. This
/// refreshes a timer whenever a hole is dashing and eases it out afterwards, so a
/// signal only a couple of packets long still blooms into a full flash. The frame
/// loop owns one and threads it through the world draw.
#[derive(Default)]
pub struct DashFx {
  flash: HashMap<usize, f32>,
}

impl DashFx {
  pub fn new() -> Self {
    Self::default()
  }

  /// Advances hole `id`'s timer and draws the burst around `pos`. Call it just
  /// before drawing the hole so the hole sits on top of the glow.
  pub fn burst(&mut self, id: usize, pos: SimVec2, radius: f32, dashing: bool, cam: &Camera, dt: f32) {
    let k = self.advance(id, dashing, dt);
    if k <= 0.0 {
      return;
    }
    let (x, y) = cam.at(pos);
    let base = (radius * cam.scale).max(3.0);
    // A ring that expands and fades as the flash decays, over a soft inner glow.
    // Cyan-white, so it reads as a burst over any hole colour.
    let grow = 1.15 + (1.0 - k) * 1.7;
    draw_circle(x, y, base * 1.05, Color::new(0.70, 0.90, 1.0, 0.16 * k));
    draw_circle_lines(x, y, base * grow, 3.0, Color::new(0.75, 0.95, 1.0, 0.85 * k));
  }

  /// Refreshes the timer to full while dashing and eases it out otherwise,
  /// returning it as a 0..1 intensity. Split from the drawing so the timing rule
  /// can be tested without a GL context.
  fn advance(&mut self, id: usize, dashing: bool, dt: f32) -> f32 {
    let timer = self.flash.entry(id).or_insert(0.0);
    *timer = if dashing { DASH_FLASH_SECS } else { (*timer - dt).max(0.0) };
    *timer / DASH_FLASH_SECS
  }
}

pub struct Camera {
  center: SimVec2,
  scale: f32,
  sw: f32,
  sh: f32,
}

impl Camera {
  /// A player's camera: centred on their hole at the default zoom.
  pub fn follow(center: SimVec2) -> Self {
    Self::viewport(center, 1.0)
  }

  /// A free camera: any centre, any zoom. `zoom` multiplies the default scale, so
  /// 1.0 is what a player sees, above 1.0 is closer in, below is further out.
  /// This is what lets an observer roam a map far bigger than one screen.
  pub fn viewport(center: SimVec2, zoom: f32) -> Self {
    let (sw, sh) = (screen_width(), screen_height());
    let scale = (sw.min(sh) * 0.5) / (VIEW_RADIUS * 1.2) * zoom;
    Self { center, scale, sw, sh }
  }

  /// World units per screen pixel at the default zoom, for turning a mouse drag
  /// in pixels into a pan in world space. Only the observer's free camera needs
  /// it, so it is absent from builds without a server.
  #[cfg(feature = "server")]
  pub fn base_scale() -> f32 {
    (screen_width().min(screen_height()) * 0.5) / (VIEW_RADIUS * 1.2)
  }

  fn at(&self, p: SimVec2) -> (f32, f32) {
    ((p.x - self.center.x) * self.scale + self.sw * 0.5, (p.y - self.center.y) * self.scale + self.sh * 0.5)
  }
}

/// How long the world takes to fade in once there is one.
///
/// Networked only: the offline build owns both sides, so its world exists from
/// the first frame and there is no transient to mask.
#[cfg(all(feature = "client", feature = "websocket"))]
const FADE_IN_SECS: f32 = 0.45;

/// Masks the join transient, which is not a cosmetic problem.
///
/// A client that is told a *field* and integrates from it has nothing at all
/// before the first packet, so the alternative to a fade is showing a world that
/// is wrong rather than merely empty. One overlay rather than an alpha threaded
/// through every draw call: it masks uniformly and cannot be forgotten by
/// whoever adds the next thing to draw.
#[cfg(all(feature = "client", feature = "websocket"))]
pub fn draw_fade_in(ready_secs: Option<f32>) {
  let alpha = match ready_secs {
    None => 1.0,
    Some(secs) => (1.0 - secs / FADE_IN_SECS).clamp(0.0, 1.0),
  };
  if alpha > 0.0 {
    draw_rectangle(0.0, 0.0, screen_width(), screen_height(), Color::new(0.0, 0.0, 0.0, alpha));
  }
}

/// A hole: the faint accretion disk (where the pull starts and pellets are still
/// escapable) and the dark core (where they are swallowed).
fn draw_hole(hole: &BlackHole, color: Color, cam: &Camera, label: Option<&str>) {
  let (x, y) = cam.at(hole.pos);
  let r = hole.radius() * cam.scale;
  draw_circle(x, y, r, C_DISK);
  draw_circle_lines(x, y, r, 1.5, color);
  draw_circle(x, y, hole.core_radius() * cam.scale, BLACK);
  draw_circle_lines(x, y, hole.core_radius() * cam.scale, 2.0, color);
  if let Some(text) = label {
    draw_text(text, x + r + 4.0, y, 18.0, color);
  }
}

pub fn draw_world(world: &World, controls: &Controls, cam: &Camera, fx: &mut DashFx, dt: f32) {
  let you = world.holes()[0].pos;

  // The server's truth, faint. Any gap to the bright pellets is divergence
  // between the local integration and the authority, and it is on by default
  // because that divergence is the entire subject of this example.
  if controls.show_ghost {
    for pellet in world.truth_pellets() {
      if pellet.pos.dist(you) <= VIEW_RADIUS * 1.15 {
        let (x, y) = cam.at(pellet.pos);
        draw_circle(x, y, 1.6, C_TRUTH);
      }
    }
  }

  // What your client believes, which under field sync it computed itself.
  for (_, pos) in world.client_render(0) {
    if pos.dist(you) <= VIEW_RADIUS * 1.15 {
      let (x, y) = cam.at(pos);
      draw_circle(x, y, 2.1, C_PELLET);
    }
  }

  for (i, hole) in world.holes().iter().enumerate() {
    if i == 0 || !hole.alive {
      continue;
    }
    if hole.pos.dist(you) <= VIEW_RADIUS * 1.6 {
      let label = format!("P{i}  {:.0}", hole.mass);
      fx.burst(i, hole.pos, hole.radius(), world.is_dashing(i), cam, dt);
      draw_hole(hole, C_RIVAL, cam, Some(&label));
    }
  }
  let mine = world.holes()[0];
  if mine.alive {
    let label = format!("you  {:.0}", mine.mass);
    fx.burst(0, mine.pos, mine.radius(), world.is_dashing(0), cam, dt);
    draw_hole(&mine, C_YOU, cam, Some(&label));
  } else {
    let text = "eliminated, respawning";
    let dims = measure_text(text, None, 26, 1.0);
    draw_text(text, cam.sw * 0.5 - dims.width * 0.5, cam.sh * 0.5, 26.0, Color::new(1.0, 0.5, 0.4, 0.9));
  }
}

/// The whole arena: every hole and the pellet field, so the scale of what is
/// being derived from so little is visible.
pub fn draw_minimap(world: &World, cam: &Camera) {
  let size = (cam.sw.min(cam.sh) * 0.24).max(140.0);
  let pad = 12.0;
  let ox = cam.sw - size - pad;
  let oy = pad;
  let s = size / ARENA_W.max(ARENA_H);

  draw_rectangle(ox, oy, size, size, Color::new(0.0, 0.0, 0.0, 0.55));
  draw_rectangle_lines(ox, oy, size, size, 1.5, DARKGRAY);
  for pellet in world.truth_pellets() {
    draw_rectangle(ox + pellet.pos.x * s, oy + pellet.pos.y * s, 1.0, 1.0, C_TRUTH);
  }
  for (i, hole) in world.holes().iter().enumerate() {
    if !hole.alive {
      continue;
    }
    let color = if i == 0 { C_YOU } else { C_RIVAL };
    draw_circle(ox + hole.pos.x * s, oy + hole.pos.y * s, (hole.radius() * s).max(2.0), color);
  }
  draw_text("whole arena", ox, oy + size + 14.0, 15.0, GRAY);
}

/// The scoreboard. Score is pellets eaten and keeps climbing; mass is the
/// physical stat, capped and knocked back by collisions, so both are shown.
pub fn draw_scores(world: &World, cam: &Camera) {
  let mut ranked: Vec<(usize, u32, f32)> = world.scores().iter().enumerate().map(|(i, s)| (i, *s, world.holes()[i].mass)).collect();
  ranked.sort_by(|a, b| b.1.cmp(&a.1));
  for (row, (i, score, mass)) in ranked.iter().enumerate() {
    let name = if *i == 0 { "you".to_string() } else { format!("P{i}") };
    let color = if *i == 0 { C_YOU } else { C_RIVAL };
    let line = format!("{}. {name}  {score}  (mass {mass:.0})", row + 1);
    draw_text(&line, 14.0, cam.sh - 84.0 + row as f32 * 20.0, 19.0, color);
  }
}

/// Drawing what a *networked* client knows, which is strictly less than the
/// offline view.
///
/// No *pellet* truth overlay and no server-side holes, because a real client has
/// neither: pellets under field sync are never sent, so there is nothing to
/// compare its own integration against. What it does have is the field it was
/// told about, which includes where the last frame put its own hole, so it gets
/// a ghost ring for that. Received state is not a privilege.
#[cfg(all(feature = "client", feature = "websocket"))]
pub fn draw_client_world(client: &blackhole_playground::net::client::NetClient, controls: &Controls, cam: &Camera, fx: &mut DashFx, dt: f32) {
  let you = client.my_position();

  for (_, pos) in client.sim.render() {
    if pos.dist(you) <= VIEW_RADIUS * 1.15 {
      let (x, y) = cam.at(pos);
      draw_circle(x, y, 2.1, C_PELLET);
    }
  }

  let me = client.me;
  for (i, hole) in client.sim.holes.iter().enumerate() {
    if !hole.alive {
      continue;
    }
    // Your own hole is drawn at the *predicted* position, not where the last
    // packet put it: that is the entire point of predicting it.
    let mine = me.is_some_and(|m| m as usize == i);
    let mut drawn = *hole;
    if mine {
      drawn.pos = you;
    }
    // The joiner's own server ghost, which needs no privilege: `hole.pos` is
    // where the last frame put it, and that is a fact this client holds. Only
    // your own hole is drawn anywhere else, so only your own hole gets a ring.
    // The gap is your prediction error plus the one link delay the sample is
    // old, and it opens during a grapple, where collision separation between
    // holes is deliberately left unpredicted.
    if controls.show_ghost && mine && hole.pos.dist(you) <= VIEW_RADIUS * 1.6 {
      let (gx, gy) = cam.at(hole.pos);
      draw_circle_lines(gx, gy, hole.radius() * cam.scale, 1.0, C_TRUTH);
      draw_circle(gx, gy, 2.0, C_TRUTH);
    }
    if drawn.pos.dist(you) <= VIEW_RADIUS * 1.6 {
      let label = format!("{}  {:.0}", if mine { "you".to_string() } else { format!("P{i}") }, hole.mass);
      let color = if mine { C_YOU } else { C_RIVAL };
      fx.burst(i, drawn.pos, drawn.radius(), client.is_dashing(i as PlayerId), cam, dt);
      draw_hole(&drawn, color, cam, Some(&label));
    }
  }
}

/// The host's omniscient view: a real client's believed field drawn over the
/// authoritative truth, exactly as the offline playground drew it.
///
/// A host is the server and a client in one process, so unlike a joiner it may
/// legitimately show both: the faint truth from the `HostView` the arena
/// publishes, and the bright believed pellets from its own client. Its own hole
/// is drawn where it is predicted, everyone else's where the server says they
/// are.
#[cfg(all(feature = "server", feature = "client", feature = "websocket"))]
pub fn draw_host_world(
  view: &blackhole_playground::net::arena::HostView,
  client: &blackhole_playground::net::client::NetClient,
  controls: &Controls,
  cam: &Camera,
  fx: &mut DashFx,
  dt: f32,
) {
  let you = client.my_position();
  let me = client.me;

  if controls.show_ghost {
    for pellet in &view.pellets {
      if pellet.pos.dist(you) <= VIEW_RADIUS * 1.15 {
        let (x, y) = cam.at(pellet.pos);
        draw_circle(x, y, 1.6, C_TRUTH);
      }
    }
  }
  for (_, pos) in client.sim.render() {
    if pos.dist(you) <= VIEW_RADIUS * 1.15 {
      let (x, y) = cam.at(pos);
      draw_circle(x, y, 2.1, C_PELLET);
    }
  }

  let mut my_hole_alive = false;
  for (i, hole) in view.holes.iter().enumerate() {
    if !hole.alive {
      continue;
    }
    let mine = me.is_some_and(|m| m as usize == i);
    let mut drawn = *hole;
    if mine {
      drawn.pos = you;
      my_hole_alive = true;
    }
    if drawn.pos.dist(you) <= VIEW_RADIUS * 1.6 {
      let label = if mine { format!("you  {:.0}", hole.mass) } else { format!("P{i}  {:.0}", hole.mass) };
      let color = if mine { C_YOU } else { C_RIVAL };
      fx.burst(i, drawn.pos, drawn.radius(), client.is_dashing(i as PlayerId), cam, dt);
      draw_hole(&drawn, color, cam, Some(&label));
    }
    // Only your own hole is drawn anywhere other than where the server has it,
    // so only your own hole gets a ghost. That single gap is this example's
    // hardest quantity: the hole is a *forced* entity, pulled by every other
    // hole and pushed out of every overlap, and collision separation is
    // deliberately left unpredicted. The ring is where the residual lives, and
    // it opens during a grapple and closes when you break away.
    //
    // A host's ring is the server's *current* truth rather than a received
    // sample, so unlike a joiner's it carries no link delay: the whole gap is
    // prediction error.
    if controls.show_ghost && mine && hole.pos.dist(you) <= VIEW_RADIUS * 1.6 {
      let (gx, gy) = cam.at(hole.pos);
      draw_circle_lines(gx, gy, hole.radius() * cam.scale, 1.0, C_TRUTH);
      draw_circle(gx, gy, 2.0, C_TRUTH);
    }
  }
  if me.is_some() && !my_hole_alive {
    let text = "eliminated, respawning";
    let dims = measure_text(text, None, 26, 1.0);
    draw_text(text, cam.sw * 0.5 - dims.width * 0.5, cam.sh * 0.5, 26.0, Color::new(1.0, 0.5, 0.4, 0.9));
  }
}

/// The whole arena from truth, with the host's own hole highlighted.
#[cfg(all(feature = "server", feature = "client", feature = "websocket"))]
pub fn draw_host_minimap(view: &blackhole_playground::net::arena::HostView, client: &blackhole_playground::net::client::NetClient, cam: &Camera) {
  let size = (cam.sw.min(cam.sh) * 0.24).max(140.0);
  let pad = 12.0;
  let ox = cam.sw - size - pad;
  let oy = pad;
  let s = size / ARENA_W.max(ARENA_H);

  draw_rectangle(ox, oy, size, size, Color::new(0.0, 0.0, 0.0, 0.55));
  draw_rectangle_lines(ox, oy, size, size, 1.5, DARKGRAY);
  for pellet in &view.pellets {
    draw_rectangle(ox + pellet.pos.x * s, oy + pellet.pos.y * s, 1.0, 1.0, C_TRUTH);
  }
  let me = client.me;
  for (i, hole) in view.holes.iter().enumerate() {
    if !hole.alive {
      continue;
    }
    let color = if me.is_some_and(|m| m as usize == i) { C_YOU } else { C_RIVAL };
    draw_circle(ox + hole.pos.x * s, oy + hole.pos.y * s, (hole.radius() * s).max(2.0), color);
  }
  draw_text("whole arena", ox, oy + size + 14.0, 15.0, GRAY);
}

/// The scoreboard from truth, ranked, with the host's own row marked.
#[cfg(all(feature = "server", feature = "client", feature = "websocket"))]
pub fn draw_host_scores(view: &blackhole_playground::net::arena::HostView, client: &blackhole_playground::net::client::NetClient, cam: &Camera) {
  let me = client.me;
  let mut ranked: Vec<(usize, u32, f32)> = view
    .scores
    .iter()
    .enumerate()
    .map(|(i, s)| (i, *s, view.holes.get(i).map(|h| h.mass).unwrap_or(0.0)))
    .collect();
  ranked.sort_by(|a, b| b.1.cmp(&a.1));
  for (row, (i, score, mass)) in ranked.iter().enumerate() {
    let mine = me.is_some_and(|m| m as usize == *i);
    let name = if mine { "you".to_string() } else { format!("P{i}") };
    let color = if mine { C_YOU } else { C_RIVAL };
    let line = format!("{}. {name}  {score}  (mass {mass:.0})", row + 1);
    draw_text(&line, 14.0, cam.sh - 84.0 + row as f32 * 20.0, 19.0, color);
  }
}

/// An observer's view: the authoritative truth, and nothing believed.
///
/// An observer runs the server but drives no hole, so unlike a host it has no
/// client and no locally integrated field to overlay. It draws the truth
/// directly and brightly, because the truth is all it has and there is no second
/// version to compare it against. Every hole is neutral; none of them is "you".
#[cfg(feature = "server")]
pub fn draw_observer_world(view: &blackhole_playground::net::arena::HostView, cam: &Camera, fx: &mut DashFx, dt: f32) {
  for pellet in &view.pellets {
    if pellet.pos.dist(cam.center) <= VIEW_RADIUS * 1.5 {
      let (x, y) = cam.at(pellet.pos);
      draw_circle(x, y, 1.9, C_PELLET);
    }
  }
  for (i, hole) in view.holes.iter().enumerate() {
    if !hole.alive {
      continue;
    }
    if hole.pos.dist(cam.center) <= VIEW_RADIUS * 1.8 {
      let label = format!("P{i}  {:.0}", hole.mass);
      let dashing = view.dashing.get(i).copied().unwrap_or(false);
      fx.burst(i, hole.pos, hole.radius(), dashing, cam, dt);
      draw_hole(hole, C_RIVAL, cam, Some(&label));
    }
  }
}

/// The whole arena from truth, for an observer.
#[cfg(feature = "server")]
pub fn draw_observer_minimap(view: &blackhole_playground::net::arena::HostView, cam: &Camera) {
  let size = (cam.sw.min(cam.sh) * 0.24).max(140.0);
  let pad = 12.0;
  let ox = cam.sw - size - pad;
  let oy = pad;
  let s = size / ARENA_W.max(ARENA_H);

  draw_rectangle(ox, oy, size, size, Color::new(0.0, 0.0, 0.0, 0.55));
  draw_rectangle_lines(ox, oy, size, size, 1.5, DARKGRAY);
  for pellet in &view.pellets {
    draw_rectangle(ox + pellet.pos.x * s, oy + pellet.pos.y * s, 1.0, 1.0, C_TRUTH);
  }
  for hole in &view.holes {
    if !hole.alive {
      continue;
    }
    draw_circle(ox + hole.pos.x * s, oy + hole.pos.y * s, (hole.radius() * s).max(2.0), C_RIVAL);
  }
  draw_text("whole arena", ox, oy + size + 14.0, 15.0, GRAY);
}

/// The scoreboard from truth, ranked, for an observer.
#[cfg(feature = "server")]
pub fn draw_observer_scores(view: &blackhole_playground::net::arena::HostView, cam: &Camera) {
  let mut ranked: Vec<(usize, u32, f32)> = view
    .scores
    .iter()
    .enumerate()
    .map(|(i, s)| (i, *s, view.holes.get(i).map(|h| h.mass).unwrap_or(0.0)))
    .collect();
  ranked.sort_by(|a, b| b.1.cmp(&a.1));
  for (row, (i, score, mass)) in ranked.iter().enumerate() {
    let line = format!("{}. P{i}  {score}  (mass {mass:.0})", row + 1);
    draw_text(&line, 14.0, cam.sh - 84.0 + row as f32 * 20.0, 19.0, C_RIVAL);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_burst_lights_on_a_dash_and_eases_out_after() {
    let dt = 1.0 / 60.0;
    let mut fx = DashFx::new();

    assert_eq!(fx.advance(0, true, dt), 1.0, "a dash lights the burst fully");

    // After it ends it decays rather than snapping off, and reaches zero within
    // the flash window.
    let after_one = fx.advance(0, false, dt);
    assert!(after_one > 0.0 && after_one < 1.0, "it eases out, not off: {after_one}");
    for _ in 0..60 {
      fx.advance(0, false, dt);
    }
    assert_eq!(fx.advance(0, false, dt), 0.0, "and settles to nothing");
  }

  #[test]
  fn a_hole_that_never_dashes_never_flashes() {
    let mut fx = DashFx::new();
    assert_eq!(fx.advance(7, false, 1.0 / 60.0), 0.0);
  }
}
