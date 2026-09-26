//! What a declared rate does to one client and not to its neighbour, over real
//! sockets and a wall clock.
//!
//! Start a host first, then run this against it:
//!
//! ```sh
//! cargo run -p horde_playground --release -- --role headless
//! cargo run -p horde_playground --release --example rate_budget -- ws://127.0.0.1:8080/ws 8192
//! ```
//!
//! Two clients join. The first declares the rate given, in bytes a second; the
//! second declares nothing. Both acknowledge what they receive, as a real
//! client does. After a warm-up that covers admission, each one's downstream
//! bytes and packets a second are averaged over ten seconds and printed.
//! Release, because a debug client cannot keep up with a 60 Hz arena.

use std::time::{Duration, Instant};

use horde_playground::net::client::NetClient;
use horde_playground::sim::types::Controls;

const WARMUP: Duration = Duration::from_secs(8);
const MEASURE: Duration = Duration::from_secs(10);
const STEP: Duration = Duration::from_millis(16);

fn main() {
  let mut args = std::env::args().skip(1);
  let url = args.next().unwrap_or_else(|| "ws://127.0.0.1:8080/ws".to_owned());
  let rate: u32 = args.next().and_then(|r| r.parse().ok()).unwrap_or(8 * 1024);
  let controls = Controls::default();

  let mut declared = NetClient::connect(&url).expect("connect the declaring client");
  let mut plain = NetClient::connect(&url).expect("connect the plain client");

  let started = Instant::now();
  let mut told = false;
  let mut next_sample = WARMUP;
  let mut sums = [0.0f64; 4];
  let mut samples = 0u32;
  while started.elapsed() < WARMUP + MEASURE {
    let now_ms = started.elapsed().as_millis() as u64;
    for client in [&mut declared, &mut plain] {
      client.poll(now_ms, &controls);
      client.tick(STEP.as_millis() as u64, &controls);
    }
    if !told && declared.is_playing() {
      declared.declare_rate(rate);
      told = true;
    }
    if started.elapsed() >= next_sample {
      next_sample += Duration::from_secs(1);
      sums[0] += declared.downstream_per_sec().0;
      sums[1] += declared.packets_per_sec();
      sums[2] += plain.downstream_per_sec().0;
      sums[3] += plain.packets_per_sec();
      samples += 1;
    }
    std::thread::sleep(STEP);
  }

  let n = f64::from(samples.max(1));
  println!("declared {rate} B/s: {:.0} B/s, {:.1} packets/s", sums[0] / n, sums[1] / n);
  println!("declared nothing:  {:.0} B/s, {:.1} packets/s", sums[2] / n, sums[3] / n);
  println!("playing: declared={} plain={}", declared.is_playing(), plain.is_playing());
}
