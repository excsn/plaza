//! A client on the real wire, shared by the desktop window and the wasm page.
//!
//! The editor's half of the collaboration argument lives here: paints are
//! applied **optimistically** into a local overlay the moment they are sent,
//! confirmed when the snapshot carries them, and **reversed** when the server
//! refuses them for want of a lock. The reversal counter is the panel's
//! number: what optimism costs when the rule is on the other machine.

use std::collections::{HashMap, VecDeque};

use plaza::app_common::locking::op_payloads::{ReleaseLockPayload, RequestLockPayload};
use plaza::app_common::object_property_ops::op_payloads::SetObjectPropertyPayload;
use plaza::app_common::ordered_collection_ops::op_payloads::InsertListItemPayload;
use plaza::app_common::presence::op_payloads::UpdatePresencePayload;
use plaza::app_common::presence::payload_fragments::{ActivityStatusPayload, CursorPositionPayload};
use plaza_wire::{MsgPackCodec, WireCodec};
use plaza_ws::pump::{mismatch_message, Arrival, FramePump};
use plaza_ws::{Event, State};

use crate::protocol::{
  tile_key, ForgeOp, ForgePresence, ForgeView, PlayerId, Refusal, TestFrame, BOARD_OBJECT, PROTOCOL, SPAWN_LIST,
};

const WIRE: MsgPackCodec = MsgPackCodec;
const LOG_KEEP: usize = 9;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
  Connecting,
  Joined,
  Gone(String),
}

pub struct NetClient {
  pump: FramePump<MsgPackCodec>,
  pub status: Status,
  pub me: Option<PlayerId>,
  pub view: Option<ForgeView>,
  /// The latest playtest frame, while one runs.
  pub frame: Option<TestFrame>,
  /// Everyone else's cursors, as presence relays them.
  pub cursors: HashMap<PlayerId, ForgePresence>,
  pub log: VecDeque<String>,

  /// Paints shown before the server has spoken, keyed like the board.
  overlay: HashMap<String, String>,
  /// The keys of paints still in flight, oldest first.
  pending: VecDeque<String>,
  /// Optimistic paints the server refused and this client took back off the
  /// screen.
  pub reversed: u64,

  next_spawn_id: u32,
  events: Vec<Event>,
  arrivals: Vec<Arrival>,
  now_ms: u64,
}

impl NetClient {
  pub fn connect(url: &str) -> Result<Self, String> {
    Ok(Self::from_pump(FramePump::connect(url, WIRE, PROTOCOL).map_err(|e| e.to_string())?))
  }

  pub fn from_socket(socket: Box<dyn plaza_ws::Socket>) -> Self {
    Self::from_pump(FramePump::new(socket, WIRE, PROTOCOL))
  }

  fn from_pump(pump: FramePump<MsgPackCodec>) -> Self {
    Self {
      pump,
      status: Status::Connecting,
      me: None,
      view: None,
      frame: None,
      cursors: HashMap::new(),
      log: VecDeque::new(),
      overlay: HashMap::new(),
      pending: VecDeque::new(),
      reversed: 0,
      next_spawn_id: 1,
      events: Vec::new(),
      arrivals: Vec::new(),
      now_ms: 0,
    }
  }

  pub fn rtt_ms(&self) -> Option<f32> {
    self.pump.rtt_ms()
  }

  /// The tile the screen should show: optimism over truth.
  pub fn tile_at(&self, x: u8, y: u8) -> Option<&str> {
    let key = tile_key(x, y);
    self
      .overlay
      .get(&key)
      .or_else(|| self.view.as_ref().and_then(|v| v.board.get(&key)))
      .map(String::as_str)
  }

  pub fn my_lock_on(&self, region: &str) -> bool {
    match (&self.view, self.me) {
      (Some(view), Some(me)) => view.locks.iter().any(|(r, p)| r == region && *p == me),
      _ => false,
    }
  }

  /// An optimistic paint: on screen now, on the wire now, and reversed later
  /// if the lock says no.
  pub fn paint(&mut self, x: u8, y: u8, tile: &str) {
    let key = tile_key(x, y);
    self.overlay.insert(key.clone(), tile.to_string());
    self.pending.push_back(key.clone());
    self.pump.send_op(&ForgeOp::SetTile(SetObjectPropertyPayload {
      object_id: BOARD_OBJECT.to_string(),
      property_key: key,
      value: tile.to_string(),
    }));
  }

  pub fn request_lock(&mut self, region: &str) {
    self.pump.send_op(&ForgeOp::RequestLock(RequestLockPayload {
      resource_id: region.to_string(),
    }));
  }

  pub fn release_lock(&mut self, region: &str) {
    self.pump.send_op(&ForgeOp::ReleaseLock(ReleaseLockPayload {
      resource_id: region.to_string(),
    }));
  }

  pub fn add_spawn(&mut self, x: u8, y: u8) {
    let id = self.me.unwrap_or(0) * 1_000 + self.next_spawn_id;
    self.next_spawn_id += 1;
    self.pump.send_op(&ForgeOp::InsertSpawn(InsertListItemPayload {
      collection_key: SPAWN_LIST.to_string(),
      item_id: id,
      item_payload: (x, y),
      after_item_id: None,
      at_index: None,
    }));
  }

  pub fn presence(&mut self, x: f32, y: f32, painting: bool) {
    self.pump.send_op(&ForgeOp::Presence(UpdatePresencePayload {
      details: ForgePresence {
        cursor: CursorPositionPayload {
          x,
          y,
          context_id: None,
        },
        status: if painting {
          ActivityStatusPayload::Editing {
            resource_name: BOARD_OBJECT.to_string(),
          }
        } else {
          ActivityStatusPayload::Viewing {
            resource_name: BOARD_OBJECT.to_string(),
          }
        },
      },
    }));
  }

  pub fn send(&mut self, op: &ForgeOp) {
    self.pump.send_op(op);
  }

  fn note(&mut self, line: String) {
    self.log.push_front(line);
    self.log.truncate(LOG_KEEP);
  }

  pub fn poll(&mut self, now_ms: u64) {
    self.now_ms = now_ms;
    let mut events = std::mem::take(&mut self.events);
    self.pump.drain(now_ms, &mut events);
    let mut arrivals = std::mem::take(&mut self.arrivals);
    self.pump.digest(&mut events, now_ms, &mut arrivals);
    self.events = events;

    for arrival in arrivals.drain(..) {
      match arrival {
        Arrival::Opened => {
          if self.status == Status::Connecting {
            self.status = Status::Joined;
          }
        }
        Arrival::Ops(frame) => self.on_ops(frame.body()),
        Arrival::Mismatch { ours, theirs } => self.status = Status::Gone(mismatch_message(ours, theirs)),
        Arrival::Closed(reason) => self.status = Status::Gone(reason),
      }
    }
    self.arrivals = arrivals;

    if self.pump.state() == State::Closed && !matches!(self.status, Status::Gone(_)) {
      self.status = Status::Gone("connection lost".to_owned());
    }
  }

  fn on_ops(&mut self, body: &[u8]) {
    let Ok(ops) = WIRE.decode::<Vec<ForgeOp>>(body) else {
      return;
    };
    for op in ops {
      match op {
        ForgeOp::Snapshot(view) => {
          // A confirmed paint leaves the overlay; a divergent one stays until
          // its refusal lands.
          self
            .overlay
            .retain(|key, value| view.board.get(key) != Some(value));
          self.pending.retain(|key| self.overlay.contains_key(key));
          self.view = Some(*view);
        }
        ForgeOp::Frame(frame) => self.frame = Some(*frame),
        ForgeOp::YouAre(id) => {
          self.me = Some(id);
          self.note(format!("you are P{id}"));
        }
        ForgeOp::LockAcquired(notice) => {
          self.note(format!("P{} holds {}", notice.by_agent_id, notice.resource_id));
        }
        ForgeOp::LockDenied(notice) => {
          self.note(format!("{} denied: {}", notice.resource_id, notice.reason));
        }
        ForgeOp::LockReleased(notice) => {
          let by = notice
            .by_agent_id
            .map(|p| format!("P{p}"))
            .unwrap_or_else(|| "the bench".to_owned());
          self.note(format!("{} released by {by}", notice.resource_id));
        }
        ForgeOp::PresenceChanged(notice) => {
          if Some(notice.agent_id) != self.me {
            self.cursors.insert(notice.agent_id, notice.new_details);
          }
        }
        ForgeOp::Refused(Refusal::RegionNotLocked) => {
          // The optimistic paint comes back off the screen, and is counted.
          if let Some(key) = self.pending.pop_front() {
            self.overlay.remove(&key);
            self.reversed += 1;
          }
          self.note("paint refused: the region is not yours".to_owned());
        }
        ForgeOp::Refused(why) => self.note(format!("refused: {why:?}")),
        _ => {}
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use plaza_wire::frame::{self, ProtocolVersion};
  use plaza_ws::scripted::ScriptedSocket;

  use super::*;
  use crate::protocol::{ForgePhase, Meters, TILE_SOFT};

  fn feed(socket: &ScriptedSocket, ops: Vec<ForgeOp>) {
    let mut bytes = Vec::new();
    frame::begin(frame::Kind::Ops, &mut bytes);
    WIRE.encode_into(&ops, &mut bytes).unwrap();
    socket.feed_message(bytes);
  }

  fn view(board: HashMap<String, String>) -> ForgeView {
    ForgeView {
      phase: ForgePhase::Forge,
      board,
      spawns: Vec::new(),
      locks: Vec::new(),
      editors: vec![1],
      meters: Meters::default(),
      playtests_run: 0,
    }
  }

  #[test]
  fn an_optimistic_paint_shows_confirms_and_reverses() {
    let socket = ScriptedSocket::new();
    let mut client = NetClient::from_socket(Box::new(socket.clone()));
    feed(&socket, vec![ForgeOp::YouAre(1)]);
    client.poll(0);

    client.paint(2, 2, TILE_SOFT);
    assert_eq!(client.tile_at(2, 2), Some(TILE_SOFT), "shown before the server speaks");

    // Confirmed: the snapshot carries it, the overlay lets go.
    let mut board = HashMap::new();
    board.insert(tile_key(2, 2), TILE_SOFT.to_string());
    feed(&socket, vec![ForgeOp::Snapshot(Box::new(view(board)))]);
    client.poll(10);
    assert_eq!(client.tile_at(2, 2), Some(TILE_SOFT));
    assert_eq!(client.reversed, 0);

    // Refused: the next paint comes back off the screen.
    client.paint(3, 3, TILE_SOFT);
    assert_eq!(client.tile_at(3, 3), Some(TILE_SOFT));
    feed(&socket, vec![ForgeOp::Refused(Refusal::RegionNotLocked)]);
    client.poll(20);
    assert_eq!(client.tile_at(3, 3), None, "the reversal took it back");
    assert_eq!(client.reversed, 1);
  }

  #[test]
  fn a_server_on_another_wire_format_is_reported_rather_than_ignored() {
    let socket = ScriptedSocket::new();
    let mut bytes = Vec::new();
    frame::begin(frame::Kind::Hello, &mut bytes);
    WIRE.encode_into(&ProtocolVersion(PROTOCOL.wrapping_add(1)), &mut bytes).unwrap();
    socket.feed_message(bytes);

    let mut client = NetClient::from_socket(Box::new(socket.clone()));
    client.poll(0);
    assert!(matches!(client.status, Status::Gone(_)));
  }
}
