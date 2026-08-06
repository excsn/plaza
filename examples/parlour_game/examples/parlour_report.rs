//! What the field names cost on this game's real traffic.
//!
//! ```sh
//! cargo run -p plaza_example_parlour_game --release --example parlour_report
//! ```

use plaza_example_parlour_game::wire_cost::{measure_a_match, Cost};

fn row(label: &str, cost: &Cost) {
  println!(
    "{label:<12} {:>7} {:>9} {:>9} {:>9} {:>8} {:>8}",
    cost.messages,
    cost.json,
    cost.compact,
    cost.named,
    format!("{:.0}%", cost.compact_of_json() * 100.0),
    format!("{:.0}%", cost.named_of_json() * 100.0),
  );
}

#[tokio::main]
async fn main() {
  println!("Bytes on the wire for one match, by encoding.\n");
  println!(
    "{:<12} {:>7} {:>9} {:>9} {:>9} {:>8} {:>8}",
    "stream", "msgs", "json", "compact", "named", "cmp/json", "nmd/json"
  );

  for seats in [2u32, 3, 4] {
    let cost = measure_a_match(seats).await;
    println!("\n-- {seats} seats");
    row("notices", &cost.notices);
    row("snapshots", &cost.snapshots);
    row("total", &cost.total());
    println!(
      "   names cost {:+.1}% over compact overall, {:+.1}% on notices, {:+.1}% on snapshots",
      cost.total().names_premium() * 100.0,
      cost.notices.names_premium() * 100.0,
      cost.snapshots.names_premium() * 100.0,
    );
  }
}
