//! A drop-in view of a remote entity: interpolation, extrapolation, and the
//! starvation handling in between, bundled.
//!
//! Rendering an entity you do not control is always the same shape: buffer the
//! server's snapshots, draw a little in the past by interpolating, and when the
//! buffer runs dry (packet loss, a latency spike) either dead-reckon forward or
//! hold the last known state. [`RemoteView`] is that, over a [`SnapshotBuffer`]
//! and [`ExtrapolationBase`], so you `push` snapshots and `render` a state
//! without hand-detecting when the buffer has starved.
//!
//! Time is `u64` milliseconds (or ticks), the near-universal case. For a
//! `Duration` timeline, compose [`SnapshotBuffer`] and [`ExtrapolationBase`]
//! directly.

use std::fmt::Debug;

use crate::extrapolation::{Extrapolatable, ExtrapolationBase};
use crate::interpolation::{Interpolatable, SnapshotBuffer};

/// How a [`RemoteView`] resolves a render. The booleans map directly onto UI
/// toggles; a real client fixes them (interpolate and extrapolate both on).
#[derive(Debug, Clone, Copy)]
pub struct RenderOpts {
  /// Interpolate at the target time. `false` draws the raw newest snapshot,
  /// which jumps at the server rate.
  pub interpolate: bool,
  /// Dead-reckon along the last velocity when the buffer has no snapshot ahead
  /// of the target, instead of holding the newest.
  pub extrapolate: bool,
}

impl Default for RenderOpts {
  fn default() -> Self {
    Self {
      interpolate: true,
      extrapolate: true,
    }
  }
}

/// A remote entity's client-side view: a snapshot buffer plus the render-time
/// decision (interpolate / extrapolate / hold).
///
/// `Velocity` is whatever your [`Extrapolatable`] impl takes; pass it alongside
/// each snapshot so a dead-reckon has something to project along.
///
/// ```ignore
/// let mut view = RemoteView::new(12, 500); // 12 snapshots, extrapolate up to 500ms
///
/// // On each server packet for this entity:
/// view.push(packet.server_time_ms, state, velocity);
///
/// // Each frame, at the interpolation target (see InterpolationClock):
/// if let Some(s) = view.render(clock.target(), RenderOpts::default()) {
///   draw(&s);
/// }
/// ```
#[derive(Debug, Clone)]
pub struct RemoteView<State: Clone + Debug, Velocity: Clone + Debug> {
  buffer: SnapshotBuffer<u64, State>,
  latest: Option<(u64, State, Velocity)>,
  max_extrapolation_ms: u64,
}

impl<State, Velocity> RemoteView<State, Velocity>
where
  State: Interpolatable<u64> + Extrapolatable<Velocity, f32> + Clone + Debug,
  Velocity: Clone + Debug,
{
  /// `buffer_size` snapshots (must be at least 2), dead-reckoning at most
  /// `max_extrapolation_ms` past the newest before holding.
  pub fn new(buffer_size: usize, max_extrapolation_ms: u64) -> Self {
    Self {
      buffer: SnapshotBuffer::new(buffer_size),
      latest: None,
      max_extrapolation_ms,
    }
  }

  /// Records a snapshot and the velocity to dead-reckon along if the buffer
  /// later starves. Call once per server packet for this entity.
  ///
  /// A snapshot older than one already seen (packets reorder under jitter) still
  /// goes into the buffer for interpolation, but does not become the "newest",
  /// so a late straggler cannot drag the extrapolation base backwards.
  pub fn push(&mut self, time_ms: u64, state: State, velocity: Velocity) {
    self.buffer.add_snapshot(time_ms, state.clone());
    if self.latest.as_ref().is_none_or(|(t, _, _)| time_ms >= *t) {
      self.latest = Some((time_ms, state, velocity));
    }
  }

  /// Where to draw the entity for interpolation target `target`.
  ///
  /// `None` until the first [`push`](Self::push). With interpolation off, the
  /// raw newest snapshot. Otherwise the interpolated state, dead-reckoned past
  /// the newest when the buffer has starved and `opts.extrapolate` is set.
  pub fn render(&self, target: Option<u64>, opts: RenderOpts) -> Option<State> {
    let (_, latest_state, latest_vel) = self.latest.as_ref()?;

    if !opts.interpolate {
      return Some(latest_state.clone());
    }
    let Some(t) = target else {
      return Some(latest_state.clone());
    };

    match self.buffer.latest_timestamp() {
      Some(newest) if opts.extrapolate && t > newest => {
        let base = ExtrapolationBase::new(latest_state.clone(), latest_vel.clone(), newest, newest);
        Some(
          base
            .get_extrapolated_state(t, self.max_extrapolation_ms, |ms| ms as f32 / 1000.0)
            .unwrap_or_else(|| latest_state.clone()),
        )
      }
      _ => Some(self.buffer.get_interpolated_state(t).unwrap_or_else(|| latest_state.clone())),
    }
  }

  /// The newest raw snapshot, if any.
  pub fn latest(&self) -> Option<&State> {
    self.latest.as_ref().map(|(_, s, _)| s)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[derive(Clone, Debug, PartialEq)]
  struct S {
    x: f32,
  }

  impl Interpolatable<u64> for S {
    fn interpolate(&self, other: &Self, t: f32, _a: u64, _b: u64) -> Self {
      S {
        x: self.x + (other.x - self.x) * t,
      }
    }
  }

  impl Extrapolatable<f32, f32> for S {
    fn extrapolate_with_velocity(&self, velocity: &f32, dt_secs: f32) -> Self {
      S {
        x: self.x + velocity * dt_secs,
      }
    }
  }

  fn view() -> RemoteView<S, f32> {
    RemoteView::new(8, 500)
  }

  #[test]
  fn nothing_renders_before_the_first_push() {
    let v = view();
    assert!(v.render(Some(100), RenderOpts::default()).is_none());
  }

  #[test]
  fn it_interpolates_between_snapshots() {
    let mut v = view();
    v.push(100, S { x: 10.0 }, 0.0);
    v.push(200, S { x: 20.0 }, 0.0);
    let at = v.render(Some(150), RenderOpts { interpolate: true, extrapolate: false }).unwrap();
    assert!((at.x - 15.0).abs() < 1e-3, "halfway is 15, got {}", at.x);
  }

  #[test]
  fn interpolation_off_renders_the_raw_newest() {
    let mut v = view();
    v.push(100, S { x: 10.0 }, 0.0);
    v.push(200, S { x: 20.0 }, 0.0);
    let raw = v.render(Some(150), RenderOpts { interpolate: false, extrapolate: false }).unwrap();
    assert_eq!(raw.x, 20.0, "newest snapshot, not interpolated");
  }

  #[test]
  fn it_dead_reckons_past_the_newest_when_asked() {
    let mut v = view();
    v.push(100, S { x: 0.0 }, 10.0); // velocity 10 units/sec
    v.push(200, S { x: 1.0 }, 10.0);

    // 500ms past the newest, at 10/s, projects +5.
    let far = v.render(Some(700), RenderOpts { interpolate: true, extrapolate: true }).unwrap();
    assert!((far.x - 6.0).abs() < 0.2, "extrapolated to ~6, got {}", far.x);

    // Without extrapolation, it holds the newest.
    let held = v.render(Some(700), RenderOpts { interpolate: true, extrapolate: false }).unwrap();
    assert!((held.x - 1.0).abs() < 1e-3, "held at the newest, got {}", held.x);
  }

  #[test]
  fn a_late_out_of_order_snapshot_does_not_become_the_newest() {
    let mut v = view();
    v.push(200, S { x: 20.0 }, 1.0);
    v.push(100, S { x: 10.0 }, 9.0); // a straggler for an earlier time arrives late

    // The newest (extrapolation base and raw-render source) stays the t=200 state,
    // not the late t=100 one.
    assert_eq!(v.latest().unwrap().x, 20.0);

    // The straggler still made it into the buffer, so interpolation at 150 works.
    let at = v.render(Some(150), RenderOpts { interpolate: true, extrapolate: false }).unwrap();
    assert!((at.x - 15.0).abs() < 1e-3, "interpolates across the reordered pair, got {}", at.x);
  }

  #[test]
  fn duplicate_timestamps_do_not_panic() {
    let mut v = view();
    v.push(100, S { x: 10.0 }, 0.0);
    v.push(100, S { x: 11.0 }, 0.0);
    v.push(200, S { x: 20.0 }, 0.0);
    let at = v.render(Some(150), RenderOpts::default());
    assert!(at.is_some_and(|s| s.x.is_finite()));
  }

  #[test]
  fn a_single_snapshot_renders_that_snapshot() {
    let mut v = view();
    v.push(100, S { x: 7.0 }, 0.0);
    // One snapshot cannot bracket a target, so it renders directly.
    let at = v.render(Some(150), RenderOpts::default()).unwrap();
    assert_eq!(at.x, 7.0);
  }

  #[test]
  fn extrapolation_holds_beyond_the_cap() {
    let mut v = view(); // max 500ms
    v.push(100, S { x: 0.0 }, 10.0);
    v.push(200, S { x: 1.0 }, 10.0);
    // 2s past the newest, well beyond the 500ms cap: holds at the newest rather
    // than flying off into the distance.
    let far = v.render(Some(2200), RenderOpts { interpolate: true, extrapolate: true }).unwrap();
    assert!((far.x - 1.0).abs() < 1e-3, "held at newest beyond the cap, got {}", far.x);
  }
}
