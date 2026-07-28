//! The browser half of the proof, running under macroquad's loader.
//!
//! ```sh
//! cd ws_client && ./serve.sh          # then open http://localhost:8090
//! cargo run -p plaza_ws --features native --example echo_server -- 9001
//! ```
//!
//! It must be a macroquad app, because miniquad's `mq_js_bundle.js` is precisely
//! what is being tested: that our JS plugin registers into the import object it
//! builds, and that the module it instantiates can call out. A plain wasm module
//! would prove nothing about the case that matters.
//!
//! The screen is the assertion. Green means every arm of `Event` was seen.

//! Cargo's `required-features` cannot also require a target, and the `miniquad`
//! backend only exists on `wasm32`, so the body is gated here as well. Without
//! it `--all-features` on a host target fails to compile this example.

#[cfg(not(target_arch = "wasm32"))]
fn main() {
  eprintln!("echo_web is the browser half of the proof: build it for wasm32, which ./serve.sh does.");
}

#[cfg(target_arch = "wasm32")]
use macroquad::prelude::*;
#[cfg(target_arch = "wasm32")]
use plaza_ws::{CloseReason, Event, Socket};

#[cfg(target_arch = "wasm32")]
fn window_conf() -> Conf {
  Conf {
    window_title: "plaza_ws echo".to_owned(),
    window_width: 720,
    window_height: 360,
    ..Default::default()
  }
}

#[cfg(target_arch = "wasm32")]
#[macroquad::main(window_conf)]
async fn main() {
  // Same host, so a page served next to the echo server needs no editing.
  let url = format!(
    "{}//{}",
    if web_is_secure() { "wss:" } else { "ws:" },
    option_env!("PLAZA_WS_ECHO").unwrap_or("127.0.0.1:9001")
  );

  let mut log: Vec<String> = vec![format!("connecting to {url}")];
  let mut socket = match plaza_ws::miniquad::connect(&url) {
    Ok(socket) => Some(socket),
    Err(e) => {
      log.push(format!("start failed: {e}"));
      None
    }
  };

  let (mut saw_open, mut saw_binary, mut saw_text, mut saw_close) = (false, false, false, false);
  let mut events = Vec::new();

  loop {
    if let Some(socket) = socket.as_mut() {
      socket.poll(&mut events);
      for event in events.drain(..) {
        match event {
          Event::Open => {
            saw_open = true;
            log.push("open".to_owned());
            let _ = socket.send(b"binary hello");
            let _ = socket.send_text("text hello");
          }
          Event::Message(bytes) => {
            saw_binary = true;
            log.push(format!("binary back: {}", String::from_utf8_lossy(&bytes)));
          }
          Event::Text(text) => {
            saw_text = true;
            log.push(format!("text back: {text}"));
            socket.close();
          }
          Event::Closed(reason) => {
            saw_close = matches!(reason, CloseReason::Remote { .. } | CloseReason::Local);
            log.push(format!("closed: {reason:?}"));
          }
        }
      }
    }

    clear_background(Color::new(0.06, 0.07, 0.09, 1.0));
    let passed = saw_open && saw_binary && saw_text && saw_close;
    let verdict = if passed {
      ("PASS: open, binary, text and close all arrived", GREEN)
    } else {
      ("waiting...", GRAY)
    };
    draw_text(verdict.0, 20.0, 36.0, 26.0, verdict.1);
    for (i, line) in log.iter().rev().take(9).enumerate() {
      draw_text(line, 20.0, 76.0 + i as f32 * 26.0, 20.0, LIGHTGRAY);
    }
    next_frame().await;
  }
}

/// Whether the page was served over TLS, so `wss` is required.
#[cfg(target_arch = "wasm32")]
fn web_is_secure() -> bool {
  // Not worth a JS call for a test fixture; the spike is served over plain http.
  false
}
