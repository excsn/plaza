//! One knock at the door from a `plaza_ws` client and what it came to.
//!
//! The browser page drives this; the native tests drive the same code over a
//! real socket, so the credential path `plaza_ws` ships is exercised by both.

use plaza_wire::{JsonCodec, WireCodec};
use plaza_ws::pump::{Arrival, FramePump};
use plaza_ws::{Socket, WsError};

use crate::types::{Account, ArcadeOp, Room};

/// Who is knocking, which decides the credential.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Knocker {
  Account(Account),
  /// An account the server bans at startup.
  Banned,
  /// Bytes that do not read as an account.
  Unreadable,
  /// Presents nothing and waits for the session's timer.
  Nothing,
}

/// The account `serve` bans at startup, so the ban has something to refuse.
pub const BANNED: Account = 99;

impl Knocker {
  pub fn credential(self) -> Option<Vec<u8>> {
    match self {
      Knocker::Account(account) => Some(account.to_string().into_bytes()),
      Knocker::Banned => Some(BANNED.to_string().into_bytes()),
      Knocker::Unreadable => Some(b"not an account".to_vec()),
      Knocker::Nothing => None,
    }
  }

  pub fn label(self) -> String {
    match self {
      Knocker::Account(account) => format!("account {account}"),
      Knocker::Banned => format!("banned account {BANNED}"),
      Knocker::Unreadable => "an unreadable credential".into(),
      Knocker::Nothing => "no credential".into(),
    }
  }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
  /// Connected or connecting, no verdict yet.
  Knocking,
  Inside { account: Account, seconds: u64, credits: u32 },
  /// Terminal: the close code the server sent and its reason. `None` is a
  /// close with no code.
  Closed { code: Option<u16>, why: String },
}

pub struct Visit {
  pub knocker: Knocker,
  pub status: Status,
  pub room: Option<Room>,
  /// Why the last action did nothing, cleared by the next one that works.
  pub notice: Option<String>,
  pump: FramePump<JsonCodec>,
}

impl Visit {
  /// Protocol 0 declares no version, matching the arcade's session.
  pub fn connect(url: &str, knocker: Knocker) -> Result<Self, WsError> {
    Ok(Self::from_pump(FramePump::connect(url, JsonCodec, 0)?, knocker))
  }

  pub fn over(socket: Box<dyn Socket>, knocker: Knocker) -> Self {
    Self::from_pump(FramePump::new(socket, JsonCodec, 0), knocker)
  }

  fn from_pump(pump: FramePump<JsonCodec>, knocker: Knocker) -> Self {
    let pump = match knocker.credential() {
      Some(credential) => pump.credential(credential),
      None => pump,
    };
    Self {
      knocker,
      status: Status::Knocking,
      room: None,
      notice: None,
      pump,
    }
  }

  pub fn poll(&mut self, now_ms: u64) {
    let mut arrivals = Vec::new();
    self.pump.poll(now_ms, &mut arrivals);
    for arrival in arrivals {
      match arrival {
        Arrival::Opened | Arrival::Mismatch { .. } => {}
        Arrival::Ops(frame) => {
          let ops: Vec<ArcadeOp> = JsonCodec.decode(frame.body()).unwrap_or_default();
          for op in ops {
            match op {
              ArcadeOp::Admitted { account, seconds, credits } => {
                self.status = Status::Inside { account, seconds, credits };
                self.notice = None;
              }
              ArcadeOp::NoCredit { account } => {
                self.notice = Some(format!("no credit left on account {account}: C does nothing"));
              }
              ArcadeOp::Snapshot(room) => self.room = Some(*room),
              ArcadeOp::InsertCoin | ArcadeOp::Push => {}
            }
          }
        }
        Arrival::Closed(closed) => {
          let why = closed
            .detail
            .as_deref()
            .map(|detail| String::from_utf8_lossy(detail).into_owned())
            .filter(|detail| !detail.is_empty())
            .unwrap_or(closed.reason);
          self.status = Status::Closed { code: closed.code, why };
        }
      }
    }
  }

  pub fn send(&mut self, op: ArcadeOp) {
    self.pump.send_op(&op);
  }

  pub fn leave(&mut self) {
    self.pump.close();
  }
}

#[cfg(test)]
mod tests {
  use plaza_wire::frame::{self, Goodbye, Kind};
  use plaza_ws::scripted::ScriptedSocket;
  use plaza_ws::Event;

  use super::*;

  fn framed(kind: Kind, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    frame::begin(kind, &mut out);
    out.extend_from_slice(body);
    out
  }

  #[test]
  fn each_knocker_presents_its_own_credential_or_none() {
    for (knocker, expected) in [
      (Knocker::Account(101), Some(&b"101"[..])),
      (Knocker::Banned, Some(&b"99"[..])),
      (Knocker::Unreadable, Some(&b"not an account"[..])),
      (Knocker::Nothing, None),
    ] {
      let scripted = ScriptedSocket::new();
      let mut visit = Visit::over(Box::new(scripted.clone()), knocker);
      scripted.feed(Event::Open);
      visit.poll(0);
      let credentials: Vec<Vec<u8>> = scripted
        .sent()
        .into_iter()
        .filter(|f| Kind::from_byte(f[0]) == Some(Kind::Credential))
        .map(|f| f[1..].to_vec())
        .collect();
      assert_eq!(credentials.first().map(Vec::as_slice), expected, "{knocker:?}");
    }
  }

  #[test]
  fn a_refused_coin_leaves_a_notice() {
    let scripted = ScriptedSocket::new();
    let mut visit = Visit::over(Box::new(scripted.clone()), Knocker::Account(101));
    scripted.feed(Event::Open);
    let ops = JsonCodec.encode(&vec![ArcadeOp::NoCredit { account: 101 }]).unwrap();
    scripted.feed_message(framed(Kind::Ops, &ops));
    visit.poll(0);
    assert_eq!(visit.notice.as_deref(), Some("no credit left on account 101: C does nothing"));
  }

  #[test]
  fn a_refusal_shows_the_code_and_the_doors_reason() {
    let scripted = ScriptedSocket::new();
    let mut visit = Visit::over(Box::new(scripted.clone()), Knocker::Banned);
    scripted.feed(Event::Open);
    let goodbye = Goodbye {
      code: 4403,
      detail: Some(b"banned".to_vec()),
    };
    scripted.feed_message(framed(Kind::Goodbye, &JsonCodec.encode(&goodbye).unwrap()));
    scripted.close_by_peer(4403, "");
    visit.poll(0);
    assert_eq!(
      visit.status,
      Status::Closed {
        code: Some(4403),
        why: "banned".into()
      }
    );
  }
}
