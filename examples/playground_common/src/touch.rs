//! On-screen controls, for the playgrounds that are otherwise unplayable
//! without a keyboard.
//!
//! Every one of these examples ships a browser build, so every one of them is
//! reachable from a phone. Two of them were driven entirely by `WASD` and had
//! no pointer input at all, which means the page loaded, the game ran, and
//! nothing a finger could do would move anything.
//!
//! # Why this uses touches rather than the mouse
//!
//! macroquad synthesises a left click from a touch by default, so a *tap
//! target* needs nothing special: `is_mouse_button_pressed` already fires. That
//! covers a menu and a build strip, and it is why the examples that are driven
//! by clicking were already fine.
//!
//! It does not cover a **held** control, and it does not cover two at once. The
//! simulated mouse is a single pointer, so "steer left while charging" is not
//! expressible through it. Anything that has to be held reads [`touches`]
//! directly, and takes the mouse only as one extra pointer when there are no
//! touches, so a desktop can still exercise the same code.
//!
//! # Why the controls are hidden until something touches the screen
//!
//! A thumb pad drawn over a desktop window is clutter in the one place a player
//! is looking. [`Pointers::seen_touch`] latches on the first touch the process
//! ever sees, and the controls draw from then on. A device that never produces
//! one never grows a d-pad.

use macroquad::prelude::*;

/// Where every finger is this frame.
///
/// The mouse counts as a pointer only when there are no touches: with
/// `simulate_mouse_with_touch` on, a touch also moves the mouse, and counting
/// both would make one finger read as two.
#[derive(Clone, Debug, Default)]
pub struct Pointers {
  points: Vec<Vec2>,
  touching: bool,
}

impl Pointers {
  pub fn gather() -> Self {
    let touches = touches();
    if !touches.is_empty() {
      return Self {
        points: touches.iter().map(|t| t.position).collect(),
        touching: true,
      };
    }
    let points = if is_mouse_button_down(MouseButton::Left) {
      vec![Vec2::from(mouse_position())]
    } else {
      Vec::new()
    };
    Self { points, touching: false }
  }

  pub fn is_empty(&self) -> bool {
    self.points.is_empty()
  }

  /// Whether any pointer is inside a rectangle.
  pub fn inside(&self, rect: Rect) -> bool {
    self.points.iter().any(|p| rect.contains(*p))
  }

  /// The first pointer inside a rectangle, if any.
  pub fn first_inside(&self, rect: Rect) -> Option<Vec2> {
    self.points.iter().copied().find(|p| rect.contains(*p))
  }

  /// Whether this frame's pointers came from a screen rather than a mouse.
  pub fn touching(&self) -> bool {
    self.touching
  }
}

/// Whether this process has ever seen a touch.
///
/// Latched rather than sampled, because a control that appeared and vanished
/// between taps would be worse than one that was never there.
pub fn seen_touch() -> bool {
  use std::sync::atomic::{AtomicBool, Ordering};
  static SEEN: AtomicBool = AtomicBool::new(false);
  if !touches().is_empty() {
    SEEN.store(true, Ordering::Relaxed);
  }
  SEEN.load(Ordering::Relaxed)
}

/// A direction on a four-way pad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Way {
  Up,
  Down,
  Left,
  Right,
}

/// The palette the on-screen controls draw in, so they read as an overlay
/// rather than as part of whatever game is underneath.
const IDLE: Color = Color::new(0.85, 0.88, 0.94, 0.20);
const HELD: Color = Color::new(1.00, 0.90, 0.55, 0.55);
const EDGE: Color = Color::new(0.85, 0.88, 0.94, 0.35);

/// A four-way pad, bottom left.
///
/// Four separate targets rather than an analogue stick, because the games that
/// need it take a *discrete* direction: a stick would have to be thresholded
/// back into one of four, and the threshold is a thing to get wrong.
#[derive(Clone, Copy, Debug)]
pub struct Pad {
  pub centre: Vec2,
  pub reach: f32,
}

impl Pad {
  /// Anchored to the bottom left, sized to the smaller screen dimension so it
  /// is thumb-sized on a phone and not enormous on a tablet.
  pub fn bottom_left() -> Self {
    let reach = (screen_width().min(screen_height()) * 0.13).clamp(38.0, 76.0);
    Self {
      centre: Vec2::new(reach * 1.9, screen_height() - reach * 1.9),
      reach,
    }
  }

  fn key(&self, way: Way) -> Rect {
    let s = self.reach;
    let (dx, dy) = match way {
      Way::Up => (0.0, -1.0),
      Way::Down => (0.0, 1.0),
      Way::Left => (-1.0, 0.0),
      Way::Right => (1.0, 0.0),
    };
    Rect::new(self.centre.x + dx * s - s * 0.5, self.centre.y + dy * s - s * 0.5, s, s)
  }

  /// Which way is held, if any. The first one found in a fixed order, so two
  /// thumbs on two arrows resolve the same way every time.
  pub fn held(&self, pointers: &Pointers) -> Option<Way> {
    [Way::Up, Way::Down, Way::Left, Way::Right].into_iter().find(|way| pointers.inside(self.key(*way)))
  }

  pub fn draw(&self, pointers: &Pointers) {
    for way in [Way::Up, Way::Down, Way::Left, Way::Right] {
      let rect = self.key(way);
      let on = pointers.inside(rect);
      draw_rectangle(rect.x, rect.y, rect.w, rect.h, if on { HELD } else { IDLE });
      draw_rectangle_lines(rect.x, rect.y, rect.w, rect.h, 1.5, EDGE);
      let (cx, cy) = (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5);
      let a = rect.w * 0.22;
      let dark = Color::new(0.06, 0.07, 0.09, 0.8);
      match way {
        Way::Up => draw_triangle(Vec2::new(cx, cy - a), Vec2::new(cx - a, cy + a), Vec2::new(cx + a, cy + a), dark),
        Way::Down => draw_triangle(Vec2::new(cx, cy + a), Vec2::new(cx - a, cy - a), Vec2::new(cx + a, cy - a), dark),
        Way::Left => draw_triangle(Vec2::new(cx - a, cy), Vec2::new(cx + a, cy - a), Vec2::new(cx + a, cy + a), dark),
        Way::Right => draw_triangle(Vec2::new(cx + a, cy), Vec2::new(cx - a, cy - a), Vec2::new(cx - a, cy + a), dark),
      }
    }
  }
}

/// A round on-screen button, bottom right.
#[derive(Clone, Debug)]
pub struct Button {
  pub centre: Vec2,
  pub radius: f32,
  pub label: &'static str,
}

impl Button {
  /// `slot` counts leftward from the bottom right corner, so a game with two
  /// buttons asks for slots 0 and 1 and gets them laid out without arithmetic.
  pub fn bottom_right(slot: usize, label: &'static str) -> Self {
    let radius = (screen_width().min(screen_height()) * 0.09).clamp(30.0, 58.0);
    Self {
      centre: Vec2::new(
        screen_width() - radius * 1.6 - slot as f32 * radius * 2.4,
        screen_height() - radius * 1.6,
      ),
      radius,
      label,
    }
  }

  fn rect(&self) -> Rect {
    Rect::new(
      self.centre.x - self.radius,
      self.centre.y - self.radius,
      self.radius * 2.0,
      self.radius * 2.0,
    )
  }

  pub fn held(&self, pointers: &Pointers) -> bool {
    // A square target for a round button, on purpose: a thumb that lands on the
    // corner meant to press it, and a miss is worse than a generous hit.
    pointers.inside(self.rect())
  }

  pub fn draw(&self, pointers: &Pointers) {
    let on = self.held(pointers);
    draw_circle(self.centre.x, self.centre.y, self.radius, if on { HELD } else { IDLE });
    draw_circle_lines(self.centre.x, self.centre.y, self.radius, 1.5, EDGE);
    let size = self.radius * 0.6;
    let dims = measure_text(self.label, None, size as u16, 1.0);
    draw_text(
      self.label,
      self.centre.x - dims.width * 0.5,
      self.centre.y + size * 0.35,
      size,
      Color::new(0.06, 0.07, 0.09, 0.85),
    );
  }
}

/// A floating analogue stick.
///
/// Wherever a finger first lands becomes the origin, and the drag from there is
/// the direction. Deliberately **relative** rather than "steer toward where I
/// touched": a drag delta lives in one coordinate space, so it cannot be skewed
/// by a mismatch between where the touch is reported and where the drawing
/// lands, which is what made an absolute version steer by raw viewport
/// position. Lifting resets it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stick {
  origin: Option<Vec2>,
}

impl Stick {
  /// The direction being asked for, as a unit vector, or zero.
  ///
  /// `dead` is the throw below which a still thumb reads as nothing, in pixels.
  pub fn dir(&mut self, pointers: &Pointers, dead: f32) -> Vec2 {
    let Some(at) = pointers.points.first().copied() else {
      self.origin = None;
      return Vec2::ZERO;
    };
    let origin = *self.origin.get_or_insert(at);
    let delta = at - origin;
    let len = delta.length();
    if len <= dead {
      return Vec2::ZERO;
    }
    delta / len
  }

  pub fn draw(&self, pointers: &Pointers) {
    let (Some(origin), Some(at)) = (self.origin, pointers.points.first().copied()) else {
      return;
    };
    draw_circle_lines(origin.x, origin.y, 46.0, 2.0, EDGE);
    draw_circle(at.x, at.y, 22.0, HELD);
  }
}
