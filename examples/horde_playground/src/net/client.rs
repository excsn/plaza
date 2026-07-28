//! A client on a real wire.
//!
//! It wraps the same [`sim::Client`] the offline playground uses, so the relevance
//! mirror, the coins and the drawing are unchanged. What it adds is everything a
//! shared clock and a function argument were standing in for:
//!
//! - **Your own movement is not predicted.** The whole scene, your own marker
//!   included, is drawn from the played-out stream at one render instant; the
//!   cost is stated where it is chosen, on `Controls::playout_delay_ms`. What
//!   this client adds is naming the tick an input is *for*, from its clock
//!   estimate plus the server's declared playout depth.
//! - **The entity stream is acknowledged over the real wire.** The sim already
//!   tracks which relevance packets it holds; this sends that acknowledgement up
//!   so the server's loss recovery has something to diff against.
//! - **The area pulse arrives as a declared event.** The packet carries the
//!   pulse's server timestamp, and the ring is a pure function of it and the
//!   render instant. It was inferred from the death burst it causes once, and
//!   the inference re-fired on every recovery repeat of the same announcements.

use plaza_client_utils::clock_sync::ClockSyncEstimator;
use plaza_client_utils::{InputCoalescer, RttEstimator};
use plaza_wire::frame;
use plaza_wire::{MsgPackCodec, WireCodec};

/// One codec for the whole client. Zero-sized, so naming it costs nothing, and
/// naming it *once* is the point: the server is built with the same type, so
/// the two ends cannot drift onto different formats.
const WIRE: MsgPackCodec = MsgPackCodec;
use plaza_ws::{CloseReason, Event, Socket, State};

use crate::sim::client::Client as SimClient;
use crate::sim::protocol::{Op, ServerPolicy, PROTOCOL};
use crate::sim::types::{Controls, PlayerId, Vec2, ARENA_H, ARENA_W, SIM_DT};

/// The server's simulation step, which is the unit its ticks are counted in.
const SIM_STEP_MS: u64 = (SIM_DT * 1000.0) as u64;

/// What to tell the player about the connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
  Connecting,
  /// Connected, and the server is timing this connection before offering a seat.
  Measuring,
  Waiting,
  Playing,
  /// Connected, but there is no seat: the arena is full, or the host shrank it.
  /// Carries what it is competing for, so the screen can say more than "no".
  NoSeat { seats: usize },
  /// Measured, and this arena is the wrong one: reconnect to `endpoint`.
  Placed { room: u32, name: String, endpoint: String, measured_ms: u32 },
  /// The server measured this connection and no arena can meet its delay.
  /// Both numbers so the client can say why rather than just decline.
  Refused { measured_ms: u32, allowed_ms: u32 },
  Gone(String),
}

const PING_INTERVAL_MS: u64 = 1000;
/// When coalescing, resend the held input at least this often, so a dropped
/// direction change cannot strand the player gliding until the next keypress.
/// See [`InputCoalescer`], which is where that reasoning now lives.
const INPUT_KEEPALIVE_MS: u64 = 120;
/// How long the red damage flash is drawn for after a hit.
const HIT_FLASH_SECS: f32 = 0.35;

/// More payload messages than this in one poll means the frame loop was
/// stopped while the socket kept receiving: a hidden browser tab, a machine
/// that slept. The two streams together arrive at under thirty messages a
/// second, so this is several seconds of backlog, far past anything a running
/// frame loop can accumulate between two polls.
const BACKLOG_TRIGGER: usize = 128;
/// What survives a backlog drop: the newest messages, which describe the only
/// moments a restarted timeline can still play.
const BACKLOG_KEEP: usize = 32;

pub struct NetClient {
  socket: Box<dyn Socket>,
  /// The same client the offline build runs. Everything it does is unchanged.
  pub sim: SimClient,
  pub status: Status,
  pub me: Option<PlayerId>,
  pub policy: Option<ServerPolicy>,

  /// Your own player.
  input_seq: u64,
  /// When to actually transmit, as opposed to when to integrate. The two are
  /// deliberately different: the prediction advances every tick whatever the wire
  /// is doing, so a quiet wire is not a stuttering player.
  send_policy: InputCoalescer<Vec2>,
  rtt: RttEstimator,
  clock: ClockSyncEstimator,

  /// What this client is actually receiving, which is the number it wants and
  /// the host cannot give it. The host reports "all players", an aggregate for
  /// the whole arena; a joiner wants its own share, because that is what says
  /// whether *its* link is the problem.
  traffic: plaza_server_utils::RateMeter,
  /// The same traffic as the *server* counts it: what these packets would cost
  /// with compact ids and quantised positions, rather than what the MessagePack
  /// on the wire actually cost. Kept beside the real figure because the gap between
  /// them is the encoding's price, and it is the one number about wire cost
  /// this example never showed.
  modelled: plaza_server_utils::RateMeter,
  /// What this client *sends*. Bandwidth has two directions and every counter
  /// here measured one of them, which made "bandwidth" mean downstream by
  /// accident. Upstream is small but it is not nothing: an input every tick
  /// unless coalescing is on, plus an acknowledgement per applied frame.
  sent: plaza_server_utils::RateMeter,
  /// Reused for every outbound frame, so sending an op allocates nothing.
  out: Vec<u8>,
  /// The worst frame in a short window, not the average over a second.
  ///
  /// The rate meters beside this one answer "how much bandwidth", which is the
  /// wrong question for a hitch: a spike lasting two frames barely moves a
  /// per-second average and is exactly what a player feels. This keeps the
  /// worst single frame so a stall has somewhere to show up.
  worst: FrameCost,
  packets: plaza_server_utils::RateMeter,
  events: Vec<Event>,
  last_ping_ms: u64,
  /// The newest input seq the server has answered. Frozen while `input_seq`
  /// climbs, this is the one signal that separates "the server is refusing my
  /// inputs" from "my inputs are not reaching it", both of which play as a
  /// player who cannot move with nothing else on screen.
  last_input_ack: u64,
  /// The tick the last transmitted input named, kept beside the newest stamp
  /// seen so the panel can show where inputs aim relative to the stream. An
  /// aim at or behind the stream names closed ticks, which the server drops.
  last_input_tick: u64,
  /// Newest server stamp seen on either stream, recorded on arrival rather
  /// than play-out, so the aim readout compares against what the wire has
  /// actually delivered.
  newest_stamp_ms: u64,
  /// The last pong's raw round trip and the worst since the last resume. Raw
  /// rather than smoothed, so a pong that crossed a stall shows as itself
  /// instead of being averaged into plausibility.
  last_pong_rtt_ms: u64,
  worst_pong_rtt_ms: u64,
  pub frames_seen: u64,
  now_ms: u64,
  /// Your own health last frame, to catch the moment it drops.
  prev_health: u8,
  /// Seconds of red damage flash left to draw, refreshed when you take a hit.
  hit_flash_secs: f32,
  /// Times a resume backlog was dropped unread, and what the last drop
  /// discarded. The record of every stall this client came back from, for the
  /// panel: without it a recovery is indistinguishable from a link fault.
  resume_drops: u64,
  last_drop_msgs: u64,
  last_drop_bytes: u64,
}

/// Microseconds from whichever monotonic clock this target has.
///
/// Split by target rather than using macroquad's clock everywhere, because
/// `get_time` asserts it is on the macroquad thread and so panics in a unit
/// test, which is where most of this file is exercised.
///
/// **Browsers clamp timer precision** (Firefox to 1ms by default, for
/// fingerprinting reasons), so read the microsecond figure on a native run.
/// The byte and op counts beside it are exact everywhere and are the proximate
/// cause anyway: decode time is a function of how much arrived.
#[cfg(target_arch = "wasm32")]
fn now_micros() -> u64 {
  (macroquad::time::get_time() * 1_000_000.0) as u64
}

#[cfg(not(target_arch = "wasm32"))]
fn now_micros() -> u64 {
  use std::sync::OnceLock;
  use std::time::Instant;
  static ORIGIN: OnceLock<Instant> = OnceLock::new();
  ORIGIN.get_or_init(Instant::now).elapsed().as_micros() as u64
}

/// The most expensive frame in a rolling window: how big it was, how long it
/// took to turn into ops, and how many ops that was.
///
/// Separate from the rate meters because they answer a different question. A
/// per-second average is the wrong instrument for a hitch: a spike lasting two
/// frames barely moves it, and is exactly what a player feels.
#[derive(Default)]
pub struct FrameCost {
  window: std::collections::VecDeque<(u32, u32, u32)>,
  bytes: u32,
  ops: u32,
  /// Summed across the window rather than kept per frame, because a browser's
  /// clock cannot resolve one frame's worth of it. See [`FrameCost::mean_micros`].
  micros_total: u64,
}

impl FrameCost {
  /// How many frames the window holds. A couple of seconds at a typical send
  /// rate: long enough that a spike does not scroll away before it is read,
  /// short enough that it is about *now* rather than the whole session.
  const WINDOW: usize = 120;

  fn record(&mut self, bytes: usize, micros: u64, ops: usize) {
    let sample = (bytes as u32, micros.min(u32::MAX as u64) as u32, ops as u32);
    self.window.push_back(sample);
    self.micros_total += sample.1 as u64;
    if let Some(dropped) = (self.window.len() > Self::WINDOW).then(|| self.window.pop_front()).flatten() {
      self.micros_total = self.micros_total.saturating_sub(dropped.1 as u64);
    }
    // Recomputed rather than kept as a running maximum, so the reading falls
    // again once the spike leaves the window. A high-water mark that only ever
    // rises says a stall happened, never that it stopped.
    let (mut b, mut o) = (0, 0);
    for &(sb, _, so) in &self.window {
      b = b.max(sb);
      o = o.max(so);
    }
    (self.bytes, self.ops) = (b, o);
  }

  /// Worst bytes and worst op count in the window. Both exact, both per frame.
  pub fn worst(&self) -> (u32, u32) {
    (self.bytes, self.ops)
  }

  /// Mean decode microseconds per frame, averaged across the whole window.
  ///
  /// **A mean rather than the worst, and deliberately so.** A browser clamps
  /// timer precision for fingerprinting reasons (Firefox to 1ms by default),
  /// and `performance.now` is what macroquad's clock reads underneath. A single
  /// decode of a hundred microseconds therefore measures as either 0 or 1000,
  /// and a per-frame *maximum* reads 1000 the instant one frame rounds up: it
  /// reports the clamp, not the work, and it looks like a tenfold regression
  /// against a native run that can see the real figure.
  ///
  /// Summing the window defeats that. A hundred frames of real work totals well
  /// past the granularity, so dividing back out recovers a per-frame number
  /// that means something in both builds. The cost is that this can no longer
  /// show a single expensive frame, which is why `worst` keeps the byte and op
  /// counts: those are exact everywhere and are what a spike is made of.
  pub fn mean_micros(&self) -> f64 {
    if self.window.is_empty() {
      return 0.0;
    }
    self.micros_total as f64 / self.window.len() as f64
  }
}

impl NetClient {
  pub fn connect(url: &str) -> Result<Self, String> {
    Ok(Self::from_socket(open(url)?))
  }

  fn from_socket(socket: Box<dyn Socket>) -> Self {
    Self {
      socket,
      sim: SimClient::new(0, 1),
      status: Status::Connecting,
      me: None,
      policy: None,
      input_seq: 0,
      send_policy: InputCoalescer::new(INPUT_KEEPALIVE_MS),
      rtt: RttEstimator::new(0.15),
      clock: ClockSyncEstimator::new(32),
      traffic: plaza_server_utils::RateMeter::new(),
      modelled: plaza_server_utils::RateMeter::new(),
      sent: plaza_server_utils::RateMeter::new(),
      out: Vec::with_capacity(512),
      worst: FrameCost::default(),
      packets: plaza_server_utils::RateMeter::new(),
      events: Vec::new(),
      last_ping_ms: 0,
      last_input_ack: 0,
      last_input_tick: 0,
      newest_stamp_ms: 0,
      last_pong_rtt_ms: 0,
      worst_pong_rtt_ms: 0,
      frames_seen: 0,
      now_ms: 0,
      prev_health: crate::sim::types::PLAYER_MAX_HEALTH as u8,
      hit_flash_secs: 0.0,
      resume_drops: 0,
      last_drop_msgs: 0,
      last_drop_bytes: 0,
    }
  }

  /// Your own health, `0..=PLAYER_MAX_HEALTH`, or full before a seat is known.
  pub fn my_health(&self) -> u8 {
    self.me.and_then(|m| self.sim.player_health.get(m as usize)).copied().unwrap_or(crate::sim::types::PLAYER_MAX_HEALTH as u8)
  }

  /// How long ago you last took a hit, while the red flash is worth drawing.
  pub fn hit_flash_age(&self) -> Option<f32> {
    (self.hit_flash_secs > 0.0).then_some(HIT_FLASH_SECS - self.hit_flash_secs)
  }

  /// Where to draw your own player: **on the same timeline as everything else**.
  ///
  /// Not predicted. The client renders the world at one instant, and exempting
  /// the local player from it is the seam every other fix in this example
  /// removed: your marker would sit a render delay ahead of the enemies it is
  /// standing among, so your shots left from somewhere you were not.
  ///
  /// Drawing it from the played-out stream instead means there is nothing to
  /// correct, so the reversal stiffness has no mechanism left rather than a
  /// better cure: prediction and authority cannot disagree when there is no
  /// prediction. It also makes a recording replay to exactly what you saw, which
  /// a predicted local player can never do.
  ///
  /// What it costs is stated where it is chosen: see
  /// [`Controls::playout_delay_ms`](crate::sim::types::Controls::playout_delay_ms).
  pub fn my_position(&self) -> Vec2 {
    let me = self.me.unwrap_or(0) as usize;
    self
      .sim
      .render_at()
      .map(|at| self.sim.render_players(at))
      .and_then(|drawn| drawn.get(me).copied())
      // Never `unwrap_or_default`: the arena is measured from a corner, so the
      // origin is a view of the outside of it, and a camera that lands there
      // before the first packet opens on the wrong world. This path is not
      // reachable today, and it is the one that regressed last time.
      .unwrap_or_else(|| {
        self
          .sim
          .players()
          .get(me)
          .copied()
          .unwrap_or_else(|| Vec2::new(ARENA_W * 0.5, ARENA_H * 0.5))
      })
  }

  /// Whether there is a world worth drawing yet.
  ///
  /// A client that renders in the past has **nothing** to show until its
  /// timeline has started and a frame has been played out of it, which is one
  /// render delay after the first packet at the earliest. Drawing anyway is not
  /// an empty screen, it is a *wrong* one: entities at the origin, a camera on
  /// the corner of the arena, and then everything teleporting into place at once
  /// when the first frame lands.
  pub fn ready(&self) -> bool {
    self.frames_seen > 0 && self.sim.render_at().is_some()
  }

  pub fn rtt_ms(&self) -> Option<f32> {
    self.rtt.rtt_ms()
  }

  pub fn is_playing(&self) -> bool {
    self.status == Status::Playing
  }

  /// How long ago the last area pulse fired, at the instant on screen. The
  /// packet declares the pulse's timestamp, and the sim derives the ring from
  /// it and the frame clock; this client adds nothing.
  pub fn nova_flash_age(&self) -> Option<f32> {
    self.sim.nova_flash_age()
  }

  /// Advances the local prediction and transmits the intent, either every tick or
  /// only on change, per `controls.coalesce_input`.
  pub fn send_input(&mut self, dir: Vec2, controls: &Controls) {
    if !self.is_playing() {
      return;
    }
    self.input_seq += 1;

    self.send_policy.set_enabled(controls.coalesce_input);
    if self.send_policy.should_send(&dir, self.now_ms) {
      // The tick this input is *for*, computed the same way by every client, so
      // two players pressing at the same instant name the same one however far
      // apart their pings are. Its own estimate of the server clock, plus the
      // playout depth the server advertised, in the server's step units.
      //
      // The server decides whether that tick is still open. This is an intention,
      // not a claim.
      let server_now = self.clock.server_time_at(self.now_ms as f64).unwrap_or(self.now_ms as f64).max(0.0) as u64;
      let depth = self.policy.map(|p| p.playout_delay_ms).unwrap_or(0);
      // The clock names the tick, and the newest arrived stamp bounds it from
      // below: the server wrote that stamp, so server time is provably past it,
      // and an aim behind it is a rejection bought in advance. After a resume
      // the fit can trail the stream by hundreds of ms until its window refills
      // (measured: aim -5 ticks against a 4-tick late window, every input
      // dropped); the floor keeps those inputs inside the accepting window with
      // no clock involved at all. It only ever lifts the aim, and never past
      // the ideal: the stamp trails true server time by the one-way delay, so
      // `stamp + depth` is at most where a perfect clock would have aimed.
      let floor = (self.newest_stamp_ms + depth) / SIM_STEP_MS;
      let tick = ((server_now + depth) / SIM_STEP_MS).max(floor);
      self.last_input_tick = tick;
      self.send_op(&Op::Input { seq: self.input_seq, dx: dir.x, dy: dir.y, tick });
    }
  }

  /// Drains the socket and folds in whatever arrived. Call once per frame.
  pub fn poll(&mut self, now_ms: u64, controls: &Controls) {
    self.now_ms = now_ms;
    if now_ms.saturating_sub(self.last_ping_ms) >= PING_INTERVAL_MS && self.socket.is_open() {
      self.last_ping_ms = now_ms;
      self.send_op(&Op::Ping { origin_ms: now_ms });
    }

    self.socket.poll(&mut self.events);
    let mut events = std::mem::take(&mut self.events);
    self.drop_resume_backlog(&mut events, now_ms);
    let mut applied_a_frame = false;
    for event in events {
      match event {
        Event::Open => {
          if self.status == Status::Connecting {
            self.status = Status::Waiting;
          }
          // Before anything else: say which wire format this build speaks, so a
          // stale page is told to reload rather than half-working.
          self.send_op(&Op::Hello { protocol: PROTOCOL });
        }
        Event::Text(text) => applied_a_frame |= self.on_frame(text.as_bytes(), now_ms, controls),
        Event::Message(bytes) => applied_a_frame |= self.on_frame(&bytes, now_ms, controls),
        Event::Closed(reason) => {
          self.status = Status::Gone(match reason {
            CloseReason::Local => "you disconnected".to_owned(),
            CloseReason::Remote { code, reason } if reason.is_empty() => format!("host closed the connection ({code})"),
            CloseReason::Remote { reason, .. } => reason,
            CloseReason::Error(e) => e,
          });
        }
      }
    }
    self.events.clear();

    // Acknowledge what we now hold, so the server's loss recovery can diff
    // against a state we provably reached. On receipt, as the offline world does,
    // so the baseline advances as fast as the link allows.
    if applied_a_frame
      && let Some((newest, mask)) = self.sim.acks().encode()
    {
      self.send_op(&Op::Ack { newest, mask, digest: self.sim.last_digest() });
    }

    // A purchase is a request on the same wire as anything else. Ask once; the
    // sim tracks the pending set so it does not spam until the answer lands.
    if controls.coins && controls.auto_buy && self.is_playing()
      && let Some(upgrade) = self.sim.wants_to_buy()
    {
      self.send_op(&Op::Buy(upgrade));
    }
  }

  /// Discards all but the tail of a resume backlog, **before any of it is
  /// parsed**: see [`plaza_ws::trim_backlog`], which owns the how and the why.
  ///
  /// What stays here is what only this client knows. Whether the burst is a
  /// join or a resume (before the first frame it is a join, and a join's burst
  /// must arrive whole). What a drop means for its own state (the timeline is
  /// lost, once, deliberately). And the accounting: the dropped bytes still
  /// crossed the wire, so the meters count them in full. The one thing not
  /// repaired by the recovery contract is a policy change made mid-stall,
  /// which stands until the host next edits a setting; accepted, since the
  /// alternative is parsing the backlog to look for it.
  fn drop_resume_backlog(&mut self, events: &mut Vec<Event>, now_ms: u64) {
    if self.frames_seen == 0 {
      return;
    }
    let Some(dropped) = plaza_ws::trim_backlog(events, BACKLOG_TRIGGER, BACKLOG_KEEP) else {
      return;
    };
    self.traffic.add(dropped.bytes);
    self.packets.add(dropped.messages);
    self.resume_drops += 1;
    self.last_drop_msgs = dropped.messages;
    self.last_drop_bytes = dropped.bytes;
    let server_now = self.clock.server_time_at(now_ms as f64).unwrap_or(now_ms as f64).max(0.0) as u64;
    self.sim.timeline_lost(server_now);
    // One line per resume, so the panel readouts that follow have a timestamped
    // anchor in the console without any per-frame logging.
    let offset = self.clock.server_time_at(now_ms as f64).map_or("unsynced".to_owned(), |s| format!("{:.0}", s - now_ms as f64));
    eprintln!(
      "resume at local {now_ms} ms: dropped {} msgs ({:.1} KiB) unread; clock offset {offset} ms over {} pongs, last pong rtt {} ms, input seq {} acked {}",
      dropped.messages,
      dropped.bytes as f64 / 1024.0,
      self.clock.sample_count(),
      self.last_pong_rtt_ms,
      self.input_seq,
      self.last_input_ack,
    );
    self.worst_pong_rtt_ms = 0;
  }

  /// Times a resume backlog was dropped unread.
  pub fn resume_drops(&self) -> u64 {
    self.resume_drops
  }

  /// The input round trip as `(seq named, newest acked)`. An acked value frozen
  /// under a climbing seq is the wire saying inputs are not landing.
  pub fn input_ack_lag(&self) -> (u64, u64) {
    (self.input_seq, self.last_input_ack)
  }

  /// How many ticks ahead of the newest arrived stamp the last input aimed.
  /// Healthy is roughly the playout depth plus the one-way delay in ticks; at
  /// or below zero the input names a closed tick and the server drops it.
  pub fn input_aim_ticks(&self) -> i64 {
    self.last_input_tick as i64 - (self.newest_stamp_ms / SIM_STEP_MS) as i64
  }

  /// The last raw pong round trip and the worst since the last resume.
  pub fn pong_rtts(&self) -> (u64, u64) {
    (self.last_pong_rtt_ms, self.worst_pong_rtt_ms)
  }

  /// The clock fit as `(offset_ms, samples)`; the offset is `None` until two
  /// exchanges are in.
  pub fn clock_diag(&self) -> (Option<f64>, usize) {
    (
      self.clock.server_time_at(self.now_ms as f64).map(|s| s - self.now_ms as f64),
      self.clock.sample_count(),
    )
  }

  /// What the last resume drop discarded: `(messages, bytes)`.
  pub fn last_resume_drop(&self) -> Option<(u64, u64)> {
    (self.resume_drops > 0).then_some((self.last_drop_msgs, self.last_drop_bytes))
  }

  /// Bytes a second arriving at this client, over a rolling window and over the
  /// whole session, and messages a second.
  pub fn downstream_per_sec(&self) -> (f64, f64) {
    (self.traffic.per_sec(), self.traffic.lifetime_per_sec())
  }

  /// The worst frame in the recent window: bytes and ops, both exact.
  ///
  /// Read this rather than the rate meters when something *hitched*: an average
  /// over a second is precisely the thing that hides a two-frame stall.
  pub fn worst_frame(&self) -> (u32, u32) {
    self.worst.worst()
  }

  /// Mean microseconds spent decoding a frame, across the same window.
  ///
  /// A mean because a browser's clock cannot resolve one frame of it; see
  /// [`FrameCost::mean_micros`].
  pub fn decode_micros(&self) -> f64 {
    self.worst.mean_micros()
  }

  pub fn packets_per_sec(&self) -> f64 {
    self.packets.per_sec()
  }

  /// What this client's traffic would have cost with the compact encoding the
  /// server's own readout models. Divide the measured figure by this and you
  /// have what the wire format costs over the encoding it is accounted in.
  pub fn modelled_per_sec(&self) -> f64 {
    self.modelled.per_sec()
  }

  /// Bytes a second this client is sending, measured the same way as the
  /// downstream: serialised once, counted, then handed to the socket.
  pub fn upstream_per_sec(&self) -> f64 {
    self.sent.per_sec()
  }

  /// Frames one op and sends it.
  ///
  /// `send_json` would serialise internally and give nothing back to measure,
  /// so the same work happens here where the length is visible, and the frame's
  /// kind tag has to be written ahead of the body regardless.
  fn send_op(&mut self, op: &Op) {
    // The codec, not a hand-rolled `serde_json` call, so the client and the
    // server cannot drift apart on format: both name `MsgPackCodec`.
    self.out.clear();
    frame::begin(frame::Kind::Ops, &mut self.out);
    match WIRE.encode_into(&std::slice::from_ref(op), &mut self.out) {
      Ok(()) => {
        self.sent.add(self.out.len() as u64);
        let _ = self.socket.send(&self.out);
      }
      // Nowhere to log on wasm, and an op that will not serialise is a bug in
      // this build rather than a runtime condition to report.
      Err(_) => debug_assert!(false, "an op failed to serialise"),
    }
  }

  fn on_frame(&mut self, bytes: &[u8], now_ms: u64, controls: &Controls) -> bool {
    // Measured on the wire as it arrives, before decoding, so it is the cost of
    // the transport rather than of the model behind it.
    self.traffic.add(bytes.len() as u64);
    self.packets.add(1);
    // One tag byte, then the body. An unknown kind is skipped rather than
    // treated as an error: a server speaking a newer protocol may send frames
    // this build has never heard of.
    let Some((tag, body)) = frame::split(bytes) else {
      return false;
    };
    if frame::Kind::from_byte(tag) != Some(frame::Kind::Ops) {
      return false;
    }
    // Timed around the decode alone: this is the work that happens between two
    // drawn frames, so it is the part of a big packet a player can feel.
    let started = now_micros();
    let decoded = WIRE.decode::<Vec<Op>>(body);
    let elapsed = now_micros().saturating_sub(started);
    let Ok(ops) = decoded else {
      return false;
    };
    self.worst.record(bytes.len(), elapsed, ops.len());
    let mut applied_frame = false;
    for op in ops {
      match op {
        Op::Welcome { player, policy } => {
          self.me = Some(player);
          self.policy = Some(policy);
          self.sim = SimClient::new(player, policy.player_count);
          self.sim.set_render_delay(policy.render_delay_ms);
          self.status = Status::Playing;
        }
        Op::Policy(policy) => {
          self.policy = Some(policy);
          self.sim.set_render_delay(policy.render_delay_ms);
        }
        // The player stream. It arrives far more often than a frame and carries
        // no deltas, so it is deliberately outside the sequence, acknowledgement
        // and digest machinery the entity stream runs on.
        Op::Players(frame) => {
          self.modelled.add(frame.bytes() as u64);
          self.newest_stamp_ms = self.newest_stamp_ms.max(frame.server_time_ms);
          let server_now = self.clock.server_time_at(now_ms as f64).unwrap_or(now_ms as f64).max(0.0) as u64;
          self.sim.on_player_frame(&frame, server_now);
        }
        Op::Frame(packet) => {
          let _ = controls;
          self.modelled.add(packet.bytes() as u64);
          // Clock sync is driven by pongs alone, not by frames. A frame's offset
          // is only right if the one-way estimate matches the delay the frame
          // actually took, and on a *host* those disagree: pongs come straight
          // back (RTT ~0) while the impairment link holds frames, so a frame
          // sample claims an offset 80 ms off. Mixing the two made the estimate
          // wobble every ping interval and jerk the enemy projection backward.
          // Pongs are correct on a host (direct) and a remote (delayed like the
          // frames), so they are the honest source.
          // Hand the sim server time, not local time. It computes a packet's age
          // as `recv - server_time` to project a sample into the present, and that
          // is only meaningful if the two clocks agree; over a real wire they do
          // not, so give it this client's *estimate* of server-time-now. Before
          // sync converges this falls back to local time, which yields an age near
          // zero rather than a wild one.
          // Nothing else happens here. The packet is queued for its instant, so
          // everything derived from its contents (the hit flash, the pulse
          // ring) follows play-out, not arrival, or the reaction would land one
          // render delay before the thing it reacts to is visible.
          self.newest_stamp_ms = self.newest_stamp_ms.max(packet.server_time_ms);
          let server_now = self.clock.server_time_at(now_ms as f64).unwrap_or(now_ms as f64).max(0.0) as u64;
          self.sim.receive_packet(*packet, server_now);
          self.frames_seen += 1;
          applied_frame = true;
        }
        // The local player is drawn from the played-out stream like every other
        // entity, so there is no prediction to retire against; the seq is kept
        // for the panel's input round-trip readout.
        Op::InputAck { seq } => {
          self.last_input_ack = self.last_input_ack.max(seq);
        }
        Op::Refused { measured_ms, allowed_ms } => {
          self.status = Status::Refused { measured_ms, allowed_ms };
        }
        // No seat, or one taken away by a resize. Either way this client is
        // about to stop receiving packets, so it says so rather than freezing
        // on the last world it was sent.
        Op::NoSeat { seats } => {
          self.status = Status::NoSeat { seats };
        }
        // Measured and sent somewhere that can carry this link. The caller
        // reconnects; this client only reports where.
        Op::Placed { room, name, endpoint, measured_ms } => {
          self.status = Status::Placed {
            room,
            name,
            endpoint,
            measured_ms,
          };
        }
        Op::Pong { origin_ms, server_ms } => {
          let raw = now_ms.saturating_sub(origin_ms);
          self.last_pong_rtt_ms = raw;
          self.worst_pong_rtt_ms = self.worst_pong_rtt_ms.max(raw);
          self.rtt.observe_pong(origin_ms, now_ms);
          let one_way = self.rtt.one_way_ms().unwrap_or(0.0) as f64;
          let offset = (server_ms as f64 + one_way) - now_ms as f64;
          self.clock.observe(now_ms as f64, offset);
        }
        Op::Outdated { server, client } => {
          self.status = Status::Gone(format!(
            "this page was built for wire format {client} and the server speaks {server}: reload to get the current client"
          ));
        }
        Op::Input { .. } | Op::Ack { .. } | Op::Buy(_) | Op::Ping { .. } | Op::Hello { .. } => {}
      }
    }
    applied_frame
  }

  /// Advances the sim, plays out whatever is due, and reacts to what actually
  /// appeared on screen this tick.
  pub fn tick(&mut self, dt_ms: u64, controls: &Controls) {
    self.sim.tick(dt_ms, controls);
    // The meters roll their window on this client's own clock.
    self.traffic.elapsed(self.now_ms);
    self.modelled.elapsed(self.now_ms);
    self.sent.elapsed(self.now_ms);
    self.packets.elapsed(self.now_ms);
    // A drop in your own health is a hit worth flashing. Detected after
    // play-out, not at receipt, so the flash lands at the instant on screen.
    let health = self.my_health();
    if health < self.prev_health {
      self.hit_flash_secs = HIT_FLASH_SECS;
    }
    self.prev_health = health;
    if self.hit_flash_secs > 0.0 {
      self.hit_flash_secs = (self.hit_flash_secs - dt_ms as f32 / 1000.0).max(0.0);
    }
    if self.socket.state() == State::Closed && !matches!(self.status, Status::Gone(_)) {
      self.status = Status::Gone("connection lost".to_owned());
    }
  }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn open(url: &str) -> Result<Box<dyn Socket>, String> {
  plaza_ws::native::connect(url).map(|s| Box::new(s) as Box<dyn Socket>).map_err(|e| e.to_string())
}

#[cfg(all(feature = "web", target_arch = "wasm32"))]
fn open(url: &str) -> Result<Box<dyn Socket>, String> {
  plaza_ws::miniquad::connect(url).map(|s| Box::new(s) as Box<dyn Socket>).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
  use std::collections::VecDeque;
  use std::sync::Arc;

  use parking_lot::Mutex;
  
  use super::*;
  use crate::sim::types::Packet;

  /// A socket whose arrivals the test scripts: what a hidden tab's receive
  /// queue looks like from the Rust side, without a browser.
  #[derive(Clone)]
  struct ScriptedSocket(Arc<Mutex<VecDeque<Event>>>);

  impl Socket for ScriptedSocket {
    fn send(&self, _bytes: &[u8]) -> Result<(), plaza_ws::WsError> {
      Ok(())
    }
    fn send_text(&self, _text: &str) -> Result<(), plaza_ws::WsError> {
      Ok(())
    }
    fn poll(&mut self, out: &mut Vec<Event>) {
      out.extend(self.0.lock().drain(..));
    }
    fn state(&self) -> State {
      State::Open
    }
    fn close(&mut self) {}
  }

  fn envelope(ops: Vec<Op>) -> Event {
    // Built through the same codec the client decodes with, so the test cannot
    // pass while the two ends disagree about the format.
    let mut bytes = Vec::new();
    frame::begin(frame::Kind::Ops, &mut bytes);
    WIRE.encode_into(&ops, &mut bytes).unwrap();
    Event::Message(bytes)
  }

  fn frame(seq: u64, server_time_ms: u64) -> Event {
    envelope(vec![Op::Frame(Box::new(Packet {
      seq,
      server_time_ms,
      ..Default::default()
    }))])
  }

  fn policy() -> ServerPolicy {
    ServerPolicy {
      sync_hz: 16,
      sample_hz: 4,
      playout_delay_ms: 100,
      render_delay_ms: 150,
      player_sync_hz: 10,
      allow_ghost: false,
      coins: true,
      generational_ids: true,
      crowd_lod_theta: 0.0,
      relevance: true,
      enemy_count: 50,
      player_count: 4,
    }
  }

  #[test]
  fn a_resume_backlog_is_dropped_unread_and_the_timeline_restarts_once() {
    // The recovery a hidden tab actually needs. Its socket keeps receiving
    // while its frame loop does not run, so the first poll back hands over
    // minutes of traffic at once. None of it is playable, so none of it is
    // parsed: everything but the tail is dropped on message lengths alone,
    // and the timeline restarts once, deliberately, rather than by the queue
    // bound tripping every 256 packets of backlog.
    let controls = Controls::default();
    let feed = ScriptedSocket(Arc::new(Mutex::new(VecDeque::new())));
    feed.0.lock().push_back(envelope(vec![Op::Welcome { player: 0, policy: policy() }]));
    feed.0.lock().push_back(frame(1, 100));

    let mut client = NetClient::from_socket(Box::new(feed.clone()));
    client.poll(120, &controls);
    assert_eq!(client.frames_seen, 1, "the join burst is read in full");
    assert_eq!(client.resume_drops(), 0, "a join is not a resume");

    // The stall: 400 send intervals arrive in one poll.
    {
      let mut queue = feed.0.lock();
      for i in 0..400u64 {
        queue.push_back(frame(2 + i, 200 + i * 62));
      }
    }
    client.poll(40_000, &controls);

    assert_eq!(client.resume_drops(), 1, "the backlog was recognised as a resume");
    let (msgs, bytes) = client.last_resume_drop().expect("the drop is on record");
    assert_eq!(msgs, 400 - BACKLOG_KEEP as u64, "everything but the tail went unread");
    assert!(bytes > 0, "and its cost was still counted");
    assert_eq!(
      client.frames_seen,
      1 + BACKLOG_KEEP as u64,
      "only the tail was parsed"
    );
    assert_eq!(client.sim.resyncs(), 1, "one deliberate restart, not one per queue-bound trip");
  }

  #[test]
  fn an_input_cannot_aim_behind_what_the_stream_has_proven() {
    // The accepting window is four ticks wide, so an aim behind the newest
    // arrived stamp is an input the server is guaranteed to drop. Measured on
    // a resumed tab: the clock fit trailed the stream, every input aimed -5
    // ticks, and the player could not move while the acks said everything was
    // arriving. The stamp needs no sync to be a lower bound: the server wrote
    // it.
    let controls = Controls::default();
    let feed = ScriptedSocket(Arc::new(Mutex::new(VecDeque::new())));
    feed.0.lock().push_back(envelope(vec![Op::Welcome { player: 0, policy: policy() }]));
    // A stream far ahead of this client's unsynced clock, which falls back to
    // local time: the shape a resume leaves behind, at test-visible scale.
    feed.0.lock().push_back(frame(1, 100_000));
    let mut client = NetClient::from_socket(Box::new(feed.clone()));
    client.poll(500, &controls);

    client.send_input(Vec2::new(1.0, 0.0), &controls);
    let aim = client.input_aim_ticks();
    assert!(aim > 0, "the floor holds the aim ahead of the newest stamp: {aim} ticks");
  }
}
