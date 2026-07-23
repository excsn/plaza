//! Estimator lab: when the fancier clock and jitter estimators earn their keep.
//!
//! `RttEstimator` (a fixed-weight moving average) is the zero-config default and
//! is enough for most games. This runs the two heavier building blocks against
//! simple baselines on a link built to expose their edge: a client clock that
//! *drifts* against the server, and *jitter* on every packet. It is headless and
//! deterministic, run it with:
//!
//! ```sh
//! cargo run --example estimator_lab -p plaza_client_utils
//! ```
//!
//! Two comparisons print:
//!
//! 1. **Clock offset under drift**: an exponential moving average of the measured
//!    offset (no drift model) vs [`ClockSyncEstimator`] (a least-squares fit of
//!    offset *and* skew). A moving average follows a drifting offset only with a
//!    constant lag and reports no drift rate at all; the regression tracks the
//!    ramp and hands back the skew as a number.
//! 2. **Jitter estimate**: raw per-packet jitter vs a [`ScalarKalman`] smoothing
//!    of it, compared by how much variance each leaves.
//!
//! The drift here is deliberately fast so twelve seconds is enough to see it; real
//! clocks drift far slower, over minutes. And a real link is *asymmetric* (the two
//! legs differ), which adds a constant offset error no estimator can remove from
//! round trips alone, the regression still recovers the drift *rate* through it.
//! The link here is kept symmetric so that irreducible error does not muddy the
//! comparison.

use plaza_client_utils::{ClockSyncEstimator, ScalarKalman};

/// A tiny deterministic PRNG so the run repeats exactly.
struct Rng(u64);
impl Rng {
  fn new(seed: u64) -> Self {
    Self(seed | 1)
  }
  fn next_u64(&mut self) -> u64 {
    let mut x = self.0;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    self.0 = x;
    x
  }
  /// A value in `[a, b)`.
  fn range(&mut self, a: f64, b: f64) -> f64 {
    a + (b - a) * ((self.next_u64() >> 11) as f64 / (1u64 << 53) as f64)
  }
}

// The true clock model. The skew is exaggerated (20000 ppm) so the drift is
// legible in a short run; real skew is orders of magnitude smaller.
const TRUE_OFFSET0_MS: f64 = 200.0;
const TRUE_SKEW: f64 = 0.02;
fn true_server_time(local_ms: f64) -> f64 {
  local_ms + TRUE_OFFSET0_MS + TRUE_SKEW * local_ms
}
fn true_offset(local_ms: f64) -> f64 {
  TRUE_OFFSET0_MS + TRUE_SKEW * local_ms
}

// Symmetric link: each leg is a base plus jitter.
const BASE_LEG_MS: f64 = 40.0;
const JITTER_MS: f64 = 25.0;

fn main() {
  let mut rng = Rng::new(0xC0FFEE);

  let mut ema_offset: Option<f64> = None;
  const EMA_ALPHA: f64 = 0.1;
  let mut clock = ClockSyncEstimator::new(32);

  let mut kalman = ScalarKalman::new(0.5, 40.0);
  let mut min_rtt = f64::INFINITY;

  println!("estimator lab: drifting ({:.0} ppm, exaggerated), symmetric, jittery (+/-{:.0}ms) link\n", TRUE_SKEW * 1e6, JITTER_MS);
  println!("{:>7}  {:>10}  {:>10}  {:>10}  |  {:>8}  {:>8}", "local", "true off", "ema off", "regr off", "raw jit", "kalman");

  let mut ema_err = 0.0;
  let mut regr_err = 0.0;
  let (mut raw_mean, mut kal_mean) = (0.0, 0.0);
  let mut raw_sq = 0.0;
  let mut kal_sq = 0.0;
  let mut n = 0.0;

  // One exchange every 100ms of local time, for 12 seconds.
  for step in 0..120 {
    let local_send = step as f64 * 100.0;
    let d_up = BASE_LEG_MS + rng.range(0.0, JITTER_MS);
    let d_down = BASE_LEG_MS + rng.range(0.0, JITTER_MS);

    let server_recv = true_server_time(local_send + d_up);
    let local_recv = local_send + d_up + d_down;
    let rtt = d_up + d_down;

    // Baseline: EMA of the symmetric-assumption offset.
    let measured_offset = server_recv - (local_send + local_recv) / 2.0;
    ema_offset = Some(match ema_offset {
      Some(prev) => prev + (measured_offset - prev) * EMA_ALPHA,
      None => measured_offset,
    });

    // Regression: offset and skew.
    clock.observe_exchange(local_send, server_recv, local_recv);

    // Jitter: raw vs Kalman-smoothed.
    min_rtt = min_rtt.min(rtt);
    let raw_jitter = rtt - min_rtt;
    let kalman_jitter = kalman.observe(raw_jitter as f32) as f64;

    if step >= 32 {
      let t = local_recv;
      let truth = true_offset(t);
      ema_err += (ema_offset.unwrap() - truth).abs();
      regr_err += (clock.offset_at(t).unwrap() - truth).abs();
      raw_mean += raw_jitter;
      kal_mean += kalman_jitter;
      raw_sq += raw_jitter * raw_jitter;
      kal_sq += kalman_jitter * kalman_jitter;
      n += 1.0;
    }

    if step % 15 == 0 && clock.is_ready() {
      let t = local_recv;
      println!(
        "{:>7.0}  {:>10.1}  {:>10.1}  {:>10.1}  |  {:>8.1}  {:>8.1}",
        t,
        true_offset(t),
        ema_offset.unwrap(),
        clock.offset_at(t).unwrap(),
        raw_jitter,
        kalman_jitter,
      );
    }
  }

  let raw_var = raw_sq / n - (raw_mean / n).powi(2);
  let kal_var = kal_sq / n - (kal_mean / n).powi(2);

  println!("\nclock skew recovered: {:.0} ppm (true {:.0} ppm); the moving average has no skew estimate at all", clock.skew() * 1e6, TRUE_SKEW * 1e6);
  println!("mean |offset error| after warmup:  ema {:.1} ms (constant lag behind the drift)   regression {:.1} ms (tracks it)", ema_err / n, regr_err / n);
  println!("jitter variance:  raw {:.1}   kalman {:.1}  ({:.0}% reduction)", raw_var, kal_var, (1.0 - kal_var / raw_var) * 100.0);
}
