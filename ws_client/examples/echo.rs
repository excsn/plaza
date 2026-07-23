//! Proof that a backend works, on a desktop or in a browser.
//!
//! ```sh
//! # native, against any echo server
//! cargo run -p plaza_ws --features native --example echo -- ws://127.0.0.1:9001
//!
//! # browser: see ws_client/static/, which builds this same file to wasm
//! ```
//!
//! Sends one binary and one text message, prints what comes back, then closes,
//! exercising every arm of [`Event`] including a close it asked for. Deliberately
//! written as a **frame loop with a fixed budget**, not as a linear script,
//! because that is how the API is meant to be used and it is the shape that
//! would expose a `poll` that secretly blocked.

use std::thread;
use std::time::{Duration, Instant};

use plaza_ws::{CloseReason, Event, Socket, State};

fn main() {
  let url = std::env::args().nth(1).unwrap_or_else(|| "ws://127.0.0.1:9001".to_owned());
  println!("connecting to {url}");

  let mut socket = match plaza_ws::native::connect(&url) {
    Ok(socket) => socket,
    Err(e) => {
      eprintln!("could not start: {e}");
      std::process::exit(1);
    }
  };

  let mut events = Vec::new();
  let mut sent = false;
  let deadline = Instant::now() + Duration::from_secs(10);

  while Instant::now() < deadline {
    socket.poll(&mut events);
    for event in events.drain(..) {
      match event {
        Event::Open => {
          println!("open");
          socket.send(b"binary hello").expect("send");
          socket.send_text("text hello").expect("send");
          sent = true;
        }
        Event::Message(bytes) => println!("binary back: {}", String::from_utf8_lossy(&bytes)),
        Event::Text(text) => {
          println!("text back: {text}");
          // Both round trips done; ask for a clean close and wait for it.
          socket.close();
        }
        Event::Closed(CloseReason::Local) => {
          println!("closed, as asked");
          return;
        }
        Event::Closed(reason) => {
          println!("closed: {reason:?}");
          std::process::exit(if sent { 0 } else { 1 });
        }
      }
    }
    if socket.state() == State::Closed && events.is_empty() {
      break;
    }
    thread::sleep(Duration::from_millis(16));
  }

  eprintln!("timed out");
  std::process::exit(1);
}
