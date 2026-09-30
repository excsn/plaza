//! The arcade over WebSockets, with a page that knocks through `plaza_ws`.
//!
//! `./wasm-serve.sh`, then open http://127.0.0.1:8083 in a few tabs. Each tab
//! knocks as whoever you choose and shows the door's verdict and close code.

use std::sync::Arc;

use actix_web::web;
use plaza_session::host::Host;

use plaza_example_door_policy::types::{DuplicateLogin, PER_IP};
use plaza_example_door_policy::visit::BANNED;
use plaza_example_door_policy::{ws_arcade, ws_route};

const BIND: &str = "127.0.0.1:8083";
const STATIC_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/static");

#[actix_web::main]
async fn main() -> std::io::Result<()> {
  plaza_session::host::init_logging();

  // KickOldest, so the same account in a second tab closes the first with
  // 4409: an admitted connection ended by the server, not only a refusal.
  let (session, door, doorman) = ws_arcade(DuplicateLogin::KickOldest, PER_IP);
  door.ban(BANNED);

  let session = web::Data::new(session);
  let doorman = web::Data::new(Arc::clone(&doorman));
  Host::new(BIND)
    .serve_dir(Some(STATIC_DIR.to_owned()))
    .run(move |cfg| {
      cfg
        .app_data(session.clone())
        .app_data(doorman.clone())
        .route("/ws", web::get().to(ws_route));
    })
    .await
}
