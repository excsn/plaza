//! The browser side of the door: knock as someone, see what the door says.
//!
//! Built for wasm32 by `wasm-build.sh`. Every knock goes through `plaza_ws`,
//! which sends the credential right after its `Hello` and reads the server's
//! goodbye when the door closes the socket.

use macroquad::prelude::*;

use plaza_example_door_policy::types::ArcadeOp;
use plaza_example_door_policy::visit::{Knocker, Status, Visit};

const KEYS: [(KeyCode, Knocker); 8] = [
  (KeyCode::Key1, Knocker::Account(101)),
  (KeyCode::Key2, Knocker::Account(102)),
  (KeyCode::Key3, Knocker::Account(103)),
  (KeyCode::Key4, Knocker::Account(104)),
  (KeyCode::Key5, Knocker::Account(105)),
  (KeyCode::B, Knocker::Banned),
  (KeyCode::U, Knocker::Unreadable),
  (KeyCode::N, Knocker::Nothing),
];

const HELP: [&str; 7] = [
  "1-5  knock as account 101-105 (three seats; a fourth account is over capacity)",
  "B    knock as the banned account",
  "U    knock with an unreadable credential",
  "N    knock with no credential (the session closes it after 500 ms)",
  "C    insert a coin    SPACE  push    L  leave",
  "Knocking costs a credit. An account starts with 3 and gets one back every 30 s.",
  "The same account in two tabs: the newer one wins and the older one is told why.",
];

fn url() -> String {
  #[cfg(target_arch = "wasm32")]
  return plaza_ws::miniquad::page_url();
  #[cfg(not(target_arch = "wasm32"))]
  return "ws://127.0.0.1:8083/ws".into();
}

#[macroquad::main("Door policy")]
async fn main() {
  let mut visit: Option<Visit> = None;
  let mut failed: Option<String> = None;

  loop {
    let now_ms = (get_time() * 1000.0) as u64;

    for (key, knocker) in KEYS {
      if is_key_pressed(key) {
        if let Some(old) = visit.as_mut() {
          old.leave();
        }
        failed = None;
        match Visit::connect(&url(), knocker) {
          Ok(new) => visit = Some(new),
          Err(error) => {
            visit = None;
            failed = Some(format!("could not connect: {error}"));
          }
        }
      }
    }
    if let Some(v) = visit.as_mut() {
      if is_key_pressed(KeyCode::C) {
        v.send(ArcadeOp::InsertCoin);
      }
      if is_key_pressed(KeyCode::Space) {
        v.send(ArcadeOp::Push);
      }
      if is_key_pressed(KeyCode::L) {
        v.leave();
      }
      v.poll(now_ms);
    }

    let mut lines: Vec<(String, Color, f32)> = vec![("DOOR POLICY".into(), WHITE, 30.0)];
    lines.extend(HELP.iter().map(|text| (text.to_string(), GRAY, 20.0)));
    lines.push((String::new(), GRAY, 16.0));
    match (&visit, &failed) {
      (_, Some(error)) => lines.push((error.clone(), RED, 24.0)),
      (None, None) => lines.push(("press a key to knock".into(), GRAY, 24.0)),
      (Some(v), None) => {
        lines.push((format!("knocking as {}", v.knocker.label()), WHITE, 24.0));
        lines.push(match &v.status {
          Status::Knocking => ("waiting for the door".into(), YELLOW, 24.0),
          Status::Inside { account, seconds, credits } => {
            // The room is sent every tick, so its seat is the live clock.
            let seat = v.room.as_ref().and_then(|r| r.seats.iter().find(|s| s.account == *account));
            let (seconds, credits) = seat.map_or((*seconds, *credits), |s| (s.seconds_left, s.credits));
            (
              format!("admitted as account {account}: {seconds}s left, {credits} credits (C adds 6s)"),
              GREEN,
              24.0,
            )
          }
          Status::Closed { code, why } => {
            let code = code.map_or("no code".to_string(), |c| c.to_string());
            (format!("closed {code}: {why}"), RED, 24.0)
          }
        });
        if let Some(notice) = &v.notice {
          lines.push((notice.clone(), YELLOW, 22.0));
        }
        if let Some(room) = &v.room {
          lines.push((String::new(), GRAY, 12.0));
          lines.push((format!("room: {} free seats", room.free_seats), GRAY, 20.0));
          for seat in &room.seats {
            lines.push((
              format!(
                "  account {}  score {}  {}s left  {} credits",
                seat.account, seat.score, seat.seconds_left, seat.credits
              ),
              GRAY,
              20.0,
            ));
          }
        }
      }
    }

    clear_background(Color::from_rgba(17, 19, 24, 255));
    let mut y = 40.0;
    for (text, colour, size) in &lines {
      draw_text(text, 24.0, y, *size, *colour);
      y += size * 1.2;
    }

    next_frame().await;
  }
}
