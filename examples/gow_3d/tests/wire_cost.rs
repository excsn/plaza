//! What a zone costs per client in bytes.
//!
//! The example argues that a genre whose design already hides the latency
//! needs almost no netcode. That is about *complexity* and says nothing about
//! bytes, so bytes are measured separately here. A frame is assembled per
//! client from shared cell payloads, which costs some bytes in exchange for
//! cell-level relevance and a build cost that does not track the client count.
//!
//! Encoded with the codec the example actually uses, rather than counted by
//! hand from field widths. A hand count derives the same fact a second time and
//! goes stale as soon as a field is added.
//!
//! ```sh
//! cargo test -p gow_3d --test wire_cost -- --nocapture
//! ```

#![cfg(feature = "server")]

use gow_3d::casting::Ms;
use gow_3d::logic::frame_for;
use gow_3d::protocol::{Delivery, GowOp, Precision, TICK_HZ};
use gow_3d::state::{spawn_at, GowState, MAX_CHARACTERS};
use plaza_wire::{MsgPackCodec, WireCodec};

/// Bytes one client is sent per second at the tick rate.
fn per_second(bytes: usize) -> f32 {
  bytes as f32 * TICK_HZ as f32
}

/// The frame one seat would be sent, encoded. The frame the server really
/// builds, via `publish` and `frame_for`, rather than a reconstruction, which
/// would go stale as soon as a field changed.
fn encoded(state: &mut GowState, seat: u16) -> usize {
  let now = state.zone.now_ms;
  let mut published = state.zone.publication();
  state.zone.publish_at(&mut published, Precision::Absolute);
  let frame = GowOp::World(Box::new(frame_for(state, &published, Delivery::Joined, Precision::Absolute, seat, now)));
  MsgPackCodec.encode(&vec![frame]).expect("encodes").len()
}

/// A zone with `count` characters, spread as the server spreads them.
fn zone_of(count: usize) -> GowState {
  let mut state = GowState::new();
  for seat in 0..count as u16 {
    state.zone.admit(seat, spawn_at(seat));
  }
  state
}

#[test]
fn what_a_zone_costs_per_client() {
  println!("\n  one client's frame at {TICK_HZ}Hz, encoded with the codec that ships:\n");
  println!("{:>12} {:>10} {:>12} {:>14}", "in zone", "in view", "bytes", "KiB/s");

  let mut rows = Vec::new();
  for count in [8usize, 16, 32, MAX_CHARACTERS] {
    let mut state = zone_of(count);
    let mut scratch = Vec::new();
    let seen = state.zone.audience_for(0, &mut scratch).seats.len();
    let bytes = encoded(&mut state, 0);
    println!(
      "{count:>12} {seen:>10} {bytes:>12} {:>13.1}",
      per_second(bytes) / 1024.0
    );
    rows.push((count, seen, bytes));
  }

  println!("\n  the spiral spawn spreads people out, so a bigger zone is not a");
  println!("  bigger frame past the view radius: that is relevance working,");
  println!("  and it is the only reason a per-client frame is affordable.\n");

  // Cost tracks who is *in view* rather than who is in the zone. Otherwise a
  // per-client frame would be strictly worse than a broadcast, since it pays
  // the same bytes and builds them N times.
  let (_, small_seen, small_bytes) = rows[0];
  let (_, big_seen, big_bytes) = rows[rows.len() - 1];
  assert!(big_seen > small_seen, "the scene has to actually grow: {small_seen} to {big_seen}");
  let byte_growth = big_bytes as f32 / small_bytes as f32;
  let zone_growth = MAX_CHARACTERS as f32 / 8.0;
  assert!(
    byte_growth < zone_growth,
    "a frame grows with the view, not the zone: {byte_growth:.1}x bytes against {zone_growth:.1}x people"
  );
}

#[test]
fn the_server_side_total_is_measured_rather_than_multiplied() {
  // One client's frame times the client count is only an estimate: every
  // client has a different audience and the ones out at the rim of the spiral
  // see fewer people than the ones in the middle. This sums the frames the
  // server would actually build.
  let mut state = zone_of(MAX_CHARACTERS);
  let middle = encoded(&mut state, 0);

  let mut total = 0usize;
  let (mut smallest, mut largest) = (usize::MAX, 0usize);
  for seat in 0..MAX_CHARACTERS as u16 {
    let bytes = encoded(&mut state, seat);
    total += bytes;
    smallest = smallest.min(bytes);
    largest = largest.max(bytes);
  }

  let measured = per_second(total) / 1024.0;
  let estimated = per_second(middle * MAX_CHARACTERS) / 1024.0;
  println!("\n  every frame the server builds in one tick, at {MAX_CHARACTERS} connected:\n");
  println!("    measured    {measured:>8.0} KiB/s");
  println!("    estimated   {estimated:>8.0} KiB/s   (one client's frame times {MAX_CHARACTERS})");
  println!("    per client  {smallest} bytes at the thinnest, {largest} at the busiest\n");
  println!("  the rim of the spiral sees fewer people than the middle, so the");
  println!("  multiplication overstates it. Worth measuring rather than");
  println!("  reasoning about, which is the whole rule this tree keeps relearning.\n");

  // The estimate is wrong because clients differ, so check that they do.
  assert!(
    largest > smallest,
    "clients must actually differ or the multiplication would have been fine: {smallest} to {largest}"
  );
  assert!(measured > 0.0);
}

#[test]
fn a_party_across_the_zone_costs_one_entry_each() {
  // Priced separately because no other example in the tree pays this cost.
  // The members move out of view before the baseline is taken. The test used
  // to walk four members out of view and into a party in one step, so the
  // audience count never changed: four entries left the near channel and the
  // same four arrived on the subscribed one. It measured a difference anyway,
  // because MessagePack spelled `Because` as its variant name and "Subscribed"
  // is six characters longer than "Near". That six was the README's
  // per-member figure. Packed, the tag is two bits and the number went to
  // zero, which exposed it.
  let mut state = zone_of(MAX_CHARACTERS);
  for member in 1..=4u16 {
    state.zone.place(member, (400.0 + member as f32 * 10.0, 0.0, 400.0));
  }
  let alone = encoded(&mut state, 0);

  for member in 1..=4u16 {
    state.zone.parties.join(0, member);
  }
  let partied = encoded(&mut state, 0);
  let each = (partied - alone) as f32 / 4.0;

  println!("\n  a party of five, none of them in view: {alone} bytes to {partied},");
  println!("  which is {each:.0} bytes per member the distance query missed.\n");

  assert!(partied > alone, "the members really were added");
  assert!(
    each < 40.0,
    "a subscribed character is one entry, not a second frame: {each} bytes"
  );
}

#[test]
fn a_cast_costs_nothing_anyone_can_see() {
  // The cast bar is this example's main feature, so it must not double the
  // frame.
  let mut state = zone_of(16);
  let quiet = encoded(&mut state, 0);

  let mut casting = 0;
  for seat in 0..16u16 {
    if state.zone.begin_cast(seat, 0, 1500 as Ms) {
      casting += 1;
    }
  }
  assert!(casting > 0, "somebody has to be casting or this measures nothing");
  let busy = encoded(&mut state, 0);

  println!("\n  {casting} characters casting at once: {quiet} bytes to {busy}.\n");
  assert!(
    busy < quiet * 2,
    "a cast bar is a field, not a frame: {quiet} to {busy}"
  );
}
