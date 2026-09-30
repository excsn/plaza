//! The browser page's client, `plaza_ws` with a credential, against the door
//! over a real WebSocket. The page runs the same `Visit` in wasm.

use std::sync::Arc;
use std::time::{Duration, Instant};

use actix_web::{web, App, HttpServer};

use plaza_example_door_policy::door::Door;
use plaza_example_door_policy::types::{ArcadeOp, DuplicateLogin, Refusal, PER_IP, SEATS, SIGNED_IN_ELSEWHERE};
use plaza_example_door_policy::visit::{Knocker, Status, Visit, BANNED};
use plaza_example_door_policy::{ws_arcade, ws_route};

/// The arcade `serve` runs, on a free port, on its own actix system.
fn door() -> (String, Arc<Door>) {
  let (tx, rx) = std::sync::mpsc::channel();
  std::thread::spawn(move || {
    actix_web::rt::System::new().block_on(async move {
      let (session, door, doorman) = ws_arcade(DuplicateLogin::KickOldest, PER_IP);
      door.ban(BANNED);
      let session = web::Data::new(session);
      let doorman = web::Data::new(doorman);
      let server = HttpServer::new(move || {
        App::new()
          .app_data(session.clone())
          .app_data(doorman.clone())
          .route("/ws", web::get().to(ws_route))
      })
      .workers(1)
      .bind("127.0.0.1:0")
      .expect("bind");
      let addr = server.addrs()[0];
      tx.send((format!("ws://{addr}/ws"), door)).expect("hand back the address");
      server.run().await.expect("serve");
    });
  });
  rx.recv().expect("the server started")
}

/// Polls until `done` holds or three seconds pass.
fn poll_until(visit: &mut Visit, done: impl Fn(&Status) -> bool) -> Status {
  let start = Instant::now();
  while start.elapsed() < Duration::from_secs(3) && !done(&visit.status) {
    visit.poll(start.elapsed().as_millis() as u64);
    std::thread::sleep(Duration::from_millis(10));
  }
  visit.status.clone()
}

/// Connects and waits for the door's verdict.
fn knock(url: &str, knocker: Knocker) -> (Visit, Status) {
  let mut visit = Visit::connect(url, knocker).expect("connect");
  let status = poll_until(&mut visit, |s| *s != Status::Knocking);
  (visit, status)
}

fn closed_with(status: &Status) -> Option<u16> {
  match status {
    Status::Closed { code, .. } => *code,
    _ => None,
  }
}

#[test]
fn an_account_is_admitted_on_its_credential() {
  let (url, _door) = door();
  let (_visit, status) = knock(&url, Knocker::Account(101));
  assert!(matches!(status, Status::Inside { account: 101, .. }), "{status:?}");
}

#[test]
fn each_refusal_reaches_the_client_as_its_code_and_reason() {
  let (url, _door) = door();
  for (knocker, refusal) in [(Knocker::Banned, Refusal::Banned), (Knocker::Unreadable, Refusal::Unreadable)] {
    let (_visit, status) = knock(&url, knocker);
    assert_eq!(
      status,
      Status::Closed {
        code: Some(refusal.code()),
        why: refusal.as_str().into()
      },
      "{knocker:?}"
    );
  }
}

#[test]
fn a_knock_that_presents_nothing_is_closed_by_the_session() {
  let (url, _door) = door();
  let (_visit, status) = knock(&url, Knocker::Nothing);
  assert_eq!(closed_with(&status), Some(4408), "{status:?}");
}

#[test]
fn an_account_past_the_last_seat_is_over_capacity() {
  let (url, _door) = door();
  let mut inside = Vec::new();
  for account in 0..SEATS as u32 {
    let (visit, status) = knock(&url, Knocker::Account(201 + account));
    assert!(matches!(status, Status::Inside { .. }), "{status:?}");
    inside.push(visit);
  }
  let (_visit, status) = knock(&url, Knocker::Account(299));
  assert_eq!(closed_with(&status), Some(Refusal::OverCapacity.code()), "{status:?}");
}

#[test]
fn the_same_account_twice_closes_the_older_connection_and_says_why() {
  let (url, _door) = door();
  let (mut first, status) = knock(&url, Knocker::Account(7));
  assert!(matches!(status, Status::Inside { .. }), "{status:?}");

  let (_second, status) = knock(&url, Knocker::Account(7));
  assert!(matches!(status, Status::Inside { .. }), "the newcomer takes over: {status:?}");

  let status = poll_until(&mut first, |s| matches!(s, Status::Closed { .. }));
  assert_eq!(
    status,
    Status::Closed {
      code: Some(SIGNED_IN_ELSEWHERE),
      why: "signed in from somewhere else".into()
    }
  );
}

/// Credits for `account` as the room last showed them.
fn credits_of(visit: &Visit, account: u32) -> Option<u32> {
  visit.room.as_ref()?.seats.iter().find(|s| s.account == account).map(|s| s.credits)
}

fn wait(visit: &mut Visit, ms: u64) {
  let start = Instant::now();
  while start.elapsed() < Duration::from_millis(ms) {
    visit.poll(start.elapsed().as_millis() as u64);
    std::thread::sleep(Duration::from_millis(10));
  }
}

#[test]
fn a_coin_works_after_switching_accounts() {
  let (url, _door) = door();
  let (mut first, status) = knock(&url, Knocker::Account(101));
  assert!(matches!(status, Status::Inside { .. }), "{status:?}");
  // Past the last credit, as a player pressing C until nothing happens.
  for _ in 0..5 {
    first.send(ArcadeOp::InsertCoin);
    wait(&mut first, 100);
  }
  wait(&mut first, 200);
  assert_eq!(credits_of(&first, 101), Some(0));
  first.leave();

  let (mut second, status) = knock(&url, Knocker::Account(102));
  assert!(matches!(status, Status::Inside { .. }), "{status:?}");
  wait(&mut second, 300);
  assert_eq!(credits_of(&second, 102), Some(2), "arrival spent one");
  second.send(ArcadeOp::InsertCoin);
  wait(&mut second, 300);
  assert_eq!(credits_of(&second, 102), Some(1), "the coin did nothing");
}
