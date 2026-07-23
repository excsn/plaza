//! A throwaway echo server, so the `echo` example can be verified without a
//! network or a third-party endpoint.
//!
//! ```sh
//! cargo run -p plaza_ws --features native --example echo_server -- 9001
//! ```
//!
//! Blocking and one thread per connection, which is exactly what an echo server
//! should be. This is a test fixture, not a building block: the real server side
//! of plaza is `plaza_session`.

use std::net::TcpListener;
use std::thread;

fn main() {
  let port: u16 = std::env::args().nth(1).and_then(|p| p.parse().ok()).unwrap_or(9001);
  let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind");
  println!("echo server on ws://127.0.0.1:{port}");

  for stream in listener.incoming() {
    let Ok(stream) = stream else { continue };
    thread::spawn(move || {
      let Ok(mut socket) = tungstenite::accept(stream) else { return };
      loop {
        match socket.read() {
          Ok(msg) if msg.is_binary() || msg.is_text() => {
            if socket.send(msg).is_err() {
              return;
            }
          }
          Ok(_) => {}
          Err(_) => return,
        }
      }
    });
  }
}
