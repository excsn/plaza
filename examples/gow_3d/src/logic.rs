//! The tick, which is mostly a send.
//!
//! There is no simulation step here worth the name: nobody's position is
//! computed, because the clients own those, and the only thing with a clock is
//! a cast bar. What the tick actually does is answer, once per client, the
//! question this example exists to ask: **who are you told about, and why**.
//!
//! One frame cannot be built and broadcast, because two characters standing in
//! different corners of the zone have nothing in common. But the spatial
//! channel does not have to be built per client either: the zone is packed
//! once per occupied grid cell ([`Zone::publish`](crate::zone::Zone::publish)),
//! and each client's frame is the payloads its view touches plus a small
//! per-client remainder (`you`, the party's extras, the landings it can see).
//! `examples/crowd_techniques.rs` priced that at 2.6x the per-client build on
//! a spread zone and 20x on a packed one.

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::common::fsm::{FsmContext as _, OpsQueue};
use plaza::error::StateLogicError;
use plaza::session::TargetedOp;
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic};
use plaza_server_utils::{Admission, Departure};
use tracing::info;

use crate::casting::Ms;
use crate::controls::Dial;
use crate::movement::Verdict;
use crate::protocol::{frame_to_ms, Delivery, Frame, GowOp, PlayerId, Precision, You, TICK_HZ};
use crate::relevance::Seat;
use crate::state::{den_at, spawn_at, GowState};
use crate::zone::Publication;

type Ctx = OpsQueue<GowOp, PlayerId>;

/// Milliseconds a tick covers.
pub const STEP_MS: Ms = 1000 / TICK_HZ;

/// How often the zone says what it has been doing, in ticks.
///
/// A headless server has no panel, so the counters it keeps are invisible
/// unless something says them out loud. Ten seconds is often enough to watch a
/// zone and rare enough to leave in a log.
pub const REPORT_EVERY: u64 = TICK_HZ * 10;

/// How many beasts the zone keeps, which is the only content it has.
pub const BEASTS: usize = 18;

#[derive(Default)]
pub struct GowLogic {
  clock: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
  dial: Option<Dial>,
  /// Adventurers the zone seats for itself, so a lone player is not alone.
  bots: usize,
  /// Beasts the zone keeps, which is the only content it has.
  beasts: usize,
}

impl std::fmt::Debug for GowLogic {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("GowLogic")
  }
}

impl GowLogic {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn with_clock(mut self, clock: std::sync::Arc<std::sync::atomic::AtomicU64>) -> Self {
    self.clock = Some(clock);
    self
  }

  pub fn with_dial(mut self, dial: Dial) -> Self {
    self.dial = Some(dial);
    self
  }

  /// Seats the zone's own characters: adventurers to share it with, and
  /// beasts to fight. Zero of both is a bare zone, which is what the tests and
  /// the measurements want.
  pub fn with_bots(mut self, bots: usize) -> Self {
    self.bots = bots;
    self.beasts = if bots == 0 { 0 } else { BEASTS };
    self
  }

  pub fn with_beasts(mut self, beasts: usize) -> Self {
    self.beasts = beasts;
    self
  }
}

impl GowLogic {
  /// Seats the zone's own characters, once.
  ///
  /// They take roster seats exactly as a player does, so nothing downstream
  /// knows the difference, and they are deliberately absent from `agents`,
  /// which is what keeps a frame from being built and encoded for a character
  /// with no socket.
  fn populate(&self, state: &mut GowState) {
    if state.populated {
      return;
    }
    state.populated = true;

    let capacity = state.capacity;
    let room = capacity.saturating_sub(self.beasts);
    for i in 0..self.bots.min(room) {
      let id = PlayerId::MAX - i as PlayerId;
      let Admission::Seated { seat, .. } = state.roster.admit(id) else {
        break;
      };
      let seat = seat as Seat;
      let at = spawn_at(seat);
      state.zone.admit(seat, at);
      state.bots.take_seat(seat, at);
    }

    for i in 0..self.beasts {
      let id = PlayerId::MAX - (capacity + i) as PlayerId;
      let Admission::Seated { seat, .. } = state.roster.admit(id) else {
        break;
      };
      state.zone.admit_beast(seat as Seat, den_at(i));
    }

    // Two parties among the zone's own, so the second relevance channel has
    // something in it before a player has made a friend.
    let seated: Vec<Seat> = state.bots.seats().collect();
    for pair in seated.chunks(3) {
      for other in pair.iter().skip(1) {
        state.zone.parties.join(pair[0], *other);
      }
    }
  }
}

#[async_trait]
impl StateLogic<GowOp, PlayerId, GowState> for GowLogic {
  async fn process_input(
    &self,
    state: &mut GowState,
    input: LogicInput<GowOp, PlayerId>,
  ) -> Result<LogicOutput<GowOp, PlayerId>, StateLogicError> {
    let mut ctx = Ctx::new();

    match input {
      LogicInput::AgentJoined { agent } => seat_player(state, &agent, &mut ctx),
      LogicInput::AgentLeft { agent_id } => depart(state, agent_id),
      LogicInput::AgentOps { source, ops } => {
        let Some(player) = source.id_cloned() else {
          return Err(StateLogicError::InvalidOperation("ops from an unidentified agent".into()));
        };
        // A player whose seat has gone is not an error, it is a packet that
        // crossed a departure. Dropping it silently is the whole handling.
        if let Some(seat) = state.seat_of(player) {
          for op in ops {
            apply(state, player, seat, op, &mut ctx);
          }
        }
      }
      LogicInput::TimeStep { .. } => {
        // Read once a tick rather than once a frame, so a mode change lands on
        // a tick boundary and cannot split one.
        if let Some(dial) = &self.dial {
          let controls = *dial.lock();
          state.zone.authority = controls.authority;
          state.delivery = controls.delivery;
          state.precision = controls.precision;
        }
        self.populate(state);
        step_once(state, &mut ctx)
      }
    }

    if let Some(clock) = &self.clock {
      clock.store(frame_to_ms(state.tick), std::sync::atomic::Ordering::Relaxed);
    }
    Ok(LogicOutput {
      ops: ctx.into_ops(),
      ..Default::default()
    })
  }
}

fn seat_player(state: &mut GowState, agent: &Agent<PlayerId>, ctx: &mut Ctx) {
  let Some(player) = agent.id_cloned() else {
    return;
  };
  let Admission::Seated { seat, .. } = state.roster.admit(player) else {
    return;
  };
  let seat = seat as Seat;
  state.agents.insert(player, agent.clone());
  state.zone.admit(seat, spawn_at(seat));
  ctx.ops_q().push(TargetedOp::new_system_to(player, vec![GowOp::Seated { seat }]));
}

fn depart(state: &mut GowState, player: PlayerId) {
  state.agents.remove(&player);
  if let Departure::Freed { seat } = state.roster.depart(&player) {
    // Which also leaves the party, or a health bar keeps updating for somebody
    // who is not here.
    state.zone.remove(seat as Seat);
  }
}

fn apply(state: &mut GowState, player: PlayerId, seat: Seat, op: GowOp, ctx: &mut Ctx) {
  match op {
    GowOp::Moved { at, yaw } => {
      state.zone.face(seat, yaw);
      if state.zone.claim(seat, at) == Verdict::Refused {
        let held = state.zone.characters.get(&seat).map(|c| c.tracked.at).unwrap_or_default();
        ctx.ops_q().push(TargetedOp::new_system_to(player, vec![GowOp::Refused { at: held }]));
      }
    }
    GowOp::Intent { yaw, forward } => state.zone.intend(seat, yaw, forward),
    GowOp::Target { seat: at } => state.zone.aim(seat, at),
    GowOp::Cast { ability, cast_ms } => {
      state.zone.begin_cast(seat, ability, cast_ms as Ms);
    }
    GowOp::Party { seat: other } => {
      if other != seat && state.zone.characters.contains_key(&other) {
        state.zone.parties.join(seat, other);
      }
    }
    GowOp::Unparty => state.zone.parties.leave(seat),
    // Server-to-client ops arriving from a client are not a protocol error
    // worth killing a connection over, they are noise.
    GowOp::World(_) | GowOp::Cell(_) | GowOp::Seated { .. } | GowOp::Refused { .. } => {}
  }
}

fn step_once(state: &mut GowState, ctx: &mut Ctx) {
  state.tick += 1;
  if state.tick.is_multiple_of(REPORT_EVERY) {
    report(state);
  }
  let mut bots = std::mem::take(&mut state.bots);
  bots.steer(&mut state.zone, STEP_MS);
  state.bots = bots;
  state.landed = state.zone.advance(STEP_MS);
  let now = state.zone.now_ms;

  let players: Vec<(PlayerId, Seat)> = state
    .agents
    .keys()
    .filter_map(|p| state.seat_of(*p).map(|s| (*p, s)))
    .collect();

  let delivery = state.delivery;
  let precision = state.precision;
  let mut published = state.published.take().unwrap_or_else(|| state.zone.publication());
  state.zone.publish_at(&mut published, precision);

  // Under `Cells` each payload goes out once, addressed to everyone whose view
  // touches it, so the server never assembles a per-client buffer. Building
  // that recipient list is the cost the scheme pays instead, and it is charged
  // here rather than hidden: a view query makes client-to-cells and addressing
  // wants cell-to-clients.
  if delivery == Delivery::Cells {
    // Addressed by **cell pair**, never by viewer. Viewers are bucketed into
    // the cell they stand in; every viewer in one cell has the same window and
    // reads every cell in it the same way, so the near/far split is a fixed
    // offset mask rather than a distance measured per listener per cell. That
    // per-listener loop was the thing doubling the tick under `Graded`.
    let GowState { zone, viewers, audience, audience_far, .. } = &mut *state;
    viewers.clear_each();
    audience.clear_each();
    audience_far.clear_each();
    for (player, seat) in &players {
      let Some(at) = zone.characters.get(seat).map(|c| c.tracked.at) else { continue };
      let cell = zone.cell_index(at.0, at.2);
      if let Some(slot) = viewers.get_mut(cell) {
        slot.push(*player);
      }
    }

    let space = *zone.space();
    let side = space.side() as i32;
    // Taken from the quantizer rather than derived again here. A second
    // derivation of the window's half-width is exactly the drift this example
    // keeps relearning: the first attempt used `(VIEW / CELL) as i32 + 1`,
    // walked 9x9 against `cells_touching`'s 7x7, and the two deliveries
    // stopped agreeing about who was in the world.
    let reach = space.quantizer().cells_for_radius(crate::zone::VIEW) as i32;
    let graded = precision == Precision::Graded;
    for (from, watching) in viewers.occupied() {
      let (vx, vz) = space.cell_at(from);
      for dz in -reach..=reach {
        for dx in -reach..=reach {
          let (tx, tz) = (vx as i32 + dx, vz as i32 + dz);
          if tx < 0 || tz < 0 || tx >= side || tz >= side {
            continue;
          }
          let target = space.index_at(tx as u32, tz as u32);
          if published.cell(target).is_none() {
            continue;
          }
          let table = if graded && crate::zone::offset_is_coarse(dx, dz) {
            &mut *audience_far
          } else {
            &mut *audience
          };
          if let Some(slot) = table.get_mut(target) {
            // A whole bucket at a time: the copy is unavoidable because
            // `MessageTarget::Agents` needs the list materialised, but it is
            // now a memcpy rather than a hash and a square root per edge.
            slot.extend_from_slice(watching);
          }
        }
      }
    }

    for (index, payload) in published.occupied() {
      if let Some(near) = state.audience.get(index)
        && !near.is_empty()
      {
        ctx.ops_q().push(TargetedOp::new(
          plaza::agent::Agent::system(),
          plaza::session::MessageTarget::Agents(near.clone()),
          vec![GowOp::Cell(payload.clone())],
        ));
      }
      if let Some(far) = state.audience_far.get(index)
        && !far.is_empty()
        && let Some(coarse) = published.cell_for(index, f32::MAX)
      {
        ctx.ops_q().push(TargetedOp::new(
          plaza::agent::Agent::system(),
          plaza::session::MessageTarget::Agents(far.clone()),
          vec![GowOp::Cell(coarse.clone())],
        ));
      }
    }
  }

  // Assembled once per occupied *viewer-cell*, then handed out by refcount.
  // Every viewer standing in one cell is owed byte-identical bodies, so doing
  // this per viewer was O(clients) work on O(cells) information, which is the
  // shape every cost in this layer had.
  state.assembled.clear_each();
  if delivery == Delivery::Joined {
    for (_, seat) in &players {
      let Some(at) = state.zone.characters.get(seat).map(|c| c.tracked.at) else { continue };
      let cell = state.zone.cell_index(at.0, at.2);
      if state.assembled.get(cell).is_some_and(Option::is_none) {
        let blob = assemble_for_cell(state, &published, cell);
        if let Some(slot) = state.assembled.get_mut(cell) {
          *slot = Some(blob);
        }
      }
    }
  }

  // The frame goes last either way, because under `Cells` it is what tells a
  // client the tick's payloads have all arrived.
  for (player, seat) in players {
    let bodies = state
      .zone
      .characters
      .get(&seat)
      .map(|c| state.zone.cell_index(c.tracked.at.0, c.tracked.at.2))
      .and_then(|cell| state.assembled.get(cell).cloned().flatten())
      .unwrap_or_default();
    let frame = frame_from(state, &published, delivery, precision, seat, now, bodies);
    ctx
      .ops_q()
      .push(TargetedOp::new_system_to(player, vec![GowOp::World(Box::new(frame))]));
  }
  state.published = Some(published);
}

/// Says what the zone has been doing, for a server nobody is watching a panel
/// for.
///
/// The waste figure is the one worth reading: it is what the flat grid handed
/// back that the distance test then threw away, and in a stacked zone it is
/// most of it.
fn report(state: &mut GowState) {
  let zone = &mut state.zone;
  let waste = if zone.examined == 0 {
    0.0
  } else {
    (1.0 - zone.returned as f64 / zone.examined as f64) * 100.0
  };
  info!(
    tick = state.tick,
    characters = zone.characters.len(),
    authority = zone.authority.label(),
    casts_landed = zone.landed,
    revives = zone.revives,
    claims_refused = zone.refusals,
    query_waste = format!("{waste:.0}%"),
    "zone"
  );
  // Reset the query counters only, because they describe a window and the
  // others describe a session: a rate and a total read differently and mixing
  // them is how a panel starts lying slowly.
  zone.examined = 0;
  zone.returned = 0;
}

/// One client's frame: the shared cell payloads its view touches, plus the
/// per-client remainder.
///
/// Public so `examples/zone_scale.rs` times the frame the server really builds
/// rather than a second copy of it: a measurement that reconstructs its subject
/// stops measuring the moment a field moves. `published` is the tick's
/// [`Publication`]; building it once and assembling per client is the shape.
pub fn frame_for(
  state: &mut GowState,
  published: &Publication,
  delivery: Delivery,
  precision: Precision,
  seat: Seat,
  now: Ms,
) -> Frame {
  let cell = state
    .zone
    .characters
    .get(&seat)
    .map(|c| state.zone.cell_index(c.tracked.at.0, c.tracked.at.2));
  let bodies = match (delivery, cell) {
    (Delivery::Joined, Some(at)) => assemble_for_cell(state, published, at),
    _ => crate::protocol::Packed::default(),
  };
  frame_from(state, published, delivery, precision, seat, now, bodies)
}

/// The body blob every viewer standing in `cell` receives.
///
/// **Assembled once per occupied viewer-cell rather than once per viewer**,
/// which is the whole of this layer's redundancy: two viewers in one cell touch
/// the same cells, earn the same width for each, and are owed byte-identical
/// bytes. What varies per viewer is `you`, the extras and the landings, and
/// those are still built per viewer because they genuinely differ.
pub fn assemble_for_cell(state: &GowState, published: &Publication, cell: usize) -> crate::protocol::Packed {
  let zone = &state.zone;
  let (cx, cz) = zone.space().corner(cell);
  let half = crate::zone::CELL / 2.0;
  let (mx, mz) = (cx + half, cz + half);
  let mut bodies = Vec::new();
  for index in zone.cells_touching(mx, mz) {
    let (tx, tz) = zone.space().corner(index);
    let away = ((mx - (tx + half)).powi(2) + (mz - (tz + half)).powi(2)).sqrt();
    if let Some(payload) = published.cell_for(index, away) {
      bodies.extend_from_slice(payload.as_slice());
    }
  }
  crate::protocol::Packed::new(bodies)
}

#[allow(clippy::too_many_arguments)]
fn frame_from(
  state: &mut GowState,
  published: &Publication,
  delivery: Delivery,
  precision: Precision,
  seat: Seat,
  now: Ms,
  bodies: crate::protocol::Packed,
) -> Frame {
  let tick = state.tick;
  let landed = std::mem::take(&mut state.landed);
  let authority = state.zone.authority;
  let you = you_of(state, seat, now);
  let zone = &state.zone;
  let me = zone.characters.get(&seat).map(|c| c.tracked.at);

  // One self-delimiting byte string rather than one per cell: each payload
  // opens with its own count, so a reader loops until the buffer runs out, and
  // 48 of 49 envelope framings disappear. `publish_costs` priced that at 1.95x.
  let _ = published;
  let mut touched = Vec::new();
  if let Some(at) = me {
    touched.extend(zone.cells_touching(at.0, at.2));
  }

  let party: Vec<Seat> = zone.parties.of(seat).filter(|s| *s != seat).collect();
  // A member already in a touched cell would be a duplicate; one who is not is
  // out of view, or a corpse the world has let go of and the party has not.
  let mut w = plaza_wire::bits::BitWriter::new();
  let placed = |c: &crate::zone::Character| {
    (c.alive || c.still_falling(now)) && touched.contains(&zone.cell_index(c.tracked.at.0, c.tracked.at.2))
  };
  let extra: Vec<&crate::zone::Character> = party
    .iter()
    .filter_map(|s| zone.characters.get(s))
    .filter(|c| !placed(c))
    .collect();
  crate::pack::open(&mut w, extra.len());
  for character in extra {
    crate::pack::write(&mut w, &crate::zone::seen_of(character, now));
  }

  // Visible caster **or** visible victim: a bolt from somebody you cannot see
  // landing on somebody you can is an effect you should watch arrive, and one
  // from somebody you can see reaching out of your view is a swing you should
  // watch leave. A landing across the zone is not news.
  let near_enough = |s: Seat| {
    me.zip(zone.characters.get(&s))
      .is_some_and(|(at, c)| crate::movement::distance(at, c.tracked.at) <= crate::zone::VIEW)
  };
  let frame = Frame {
    tick,
    you,
    authority,
    delivery,
    precision,
    extent: zone.extent(),
    bodies: bodies,
    extras: crate::protocol::Packed::new(w.finish()),
    party,
    landed: landed
      .iter()
      .copied()
      .filter(|l| near_enough(l.seat) || l.victim.is_some_and(near_enough))
      .collect(),
  };
  state.landed = landed;
  frame
}

/// What a player is told about themselves.
///
/// Its own block rather than a lookup into the audience list, because that is
/// the defect this fixes: a client drew its own body from its own position and
/// read everything else out of the list of other people, so its cast bar, its
/// mana and its cooldown were never read at all and every key press was
/// silent. What a player must know about themselves is not a subset of what
/// they are told about anyone else.
fn you_of(state: &GowState, seat: Seat, now: Ms) -> Option<You> {
  let character = state.zone.characters.get(&seat)?;
  Some(You {
    seat,
    health: character.health,
    max_health: character.max_health,
    mana: character.mana.round() as u16,
    max_mana: crate::zone::MAX_MANA,
    casting_ms: character
      .casting
      .map(|cast| cast.lands_at.saturating_sub(now) as u32),
    casting: character.casting.map(|cast| cast.ability),
    ready_in_ms: character.ready_at.saturating_sub(now) as u32,
    up_in_ms: (!character.alive).then(|| character.up_at.saturating_sub(now) as u32),
    target: character.target,
    at: character.tracked.at,
    spawn: character.spawns,
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::protocol::Because;

  fn built(state: &mut GowState, seat: Seat, now: Ms) -> Frame {
    let mut published = state.zone.publication();
    state.zone.publish_at(&mut published, Precision::Absolute);
    frame_for(state, &published, Delivery::Joined, Precision::Absolute, seat, now)
  }

  /// Seats a player and registers them as a connected agent, so `step_once`
  /// builds and addresses for them.
  fn seat_for(state: &mut GowState, player: PlayerId) -> Seat {
    let seat = seated(state, player);
    state.agents.insert(player, Agent::new_human(player));
    seat
  }

  fn seated(state: &mut GowState, player: PlayerId) -> Seat {
    let Admission::Seated { seat, .. } = state.roster.admit(player) else {
      panic!("no seat");
    };
    let seat = seat as Seat;
    state.zone.admit(seat, spawn_at(seat));
    seat
  }

  #[tokio::test]
  async fn both_delivery_modes_describe_the_same_world() {
    // The whole reason both ship: they are two ways to move the same bodies,
    // so a client must not be able to tell which one it was sent. Anything
    // that drifts between them is a bug in one of them, and this is the test
    // that would say so.
    async fn world(delivery: Delivery) -> Vec<crate::protocol::Seen> {
      let logic = GowLogic::new().with_bots(24);
      let mut state = GowState::new();
      state.delivery = delivery;
      let watcher = seat_for(&mut state, 1);
      for _ in 0..3 {
        logic
          .process_input(&mut state, LogicInput::TimeStep {
            delta_time: std::time::Duration::from_millis(33),
          })
          .await
          .unwrap();
      }
      let out = logic
        .process_input(&mut state, LogicInput::TimeStep {
          delta_time: std::time::Duration::from_millis(33),
        })
        .await
        .unwrap();

      // Reassemble exactly as a client does: collect the cell ops addressed to
      // this seat, then let the frame that terminates the tick label them.
      let mut cells = Vec::new();
      let mut frame = None;
      for targeted in &out.ops {
        let for_me = match &targeted.target {
          plaza::session::MessageTarget::Agent(id) => *id == 1,
          plaza::session::MessageTarget::Agents(ids) => ids.contains(&1),
          _ => false,
        };
        if !for_me {
          continue;
        }
        for op in &targeted.ops {
          match op {
            GowOp::Cell(payload) => cells.push(payload.clone()),
            GowOp::World(f) => frame = Some((**f).clone()),
            _ => {}
          }
        }
      }
      let _ = watcher;
      let mut seen = frame.expect("a frame every tick").seen_with(&cells);
      seen.sort_by_key(|s| s.seat);
      seen
    }

    let joined = world(Delivery::Joined).await;
    let celled = world(Delivery::Cells).await;
    assert!(!joined.is_empty(), "the zone has to describe somebody");
    assert_eq!(
      joined.iter().map(|s| s.seat).collect::<Vec<_>>(),
      celled.iter().map(|s| s.seat).collect::<Vec<_>>(),
      "the two deliveries disagree about who is in the world"
    );
    for (a, b) in joined.iter().zip(&celled) {
      assert_eq!(a.at, b.at, "seat {} landed somewhere else", a.seat);
      assert_eq!(a.health, b.health);
      assert_eq!(a.because, b.because, "seat {} is in the frame for a different reason", a.seat);
    }
  }

  #[tokio::test]
  async fn every_precision_describes_the_same_world_within_its_own_step() {
    // Three layouts for the same bodies, so a client must not be able to tell
    // which it was sent beyond the quantiser's own step. Graded is the one
    // worth pinning: its width is chosen per *cell* and cannot be chosen per
    // viewer, so a bug there shows up as a body in the wrong place for some
    // viewers and not others.
    async fn world(precision: Precision) -> Vec<crate::protocol::Seen> {
      let logic = GowLogic::new().with_bots(24);
      let mut state = GowState::new();
      state.precision = precision;
      let _ = seat_for(&mut state, 1);
      for _ in 0..3 {
        logic
          .process_input(&mut state, LogicInput::TimeStep {
            delta_time: std::time::Duration::from_millis(33),
          })
          .await
          .unwrap();
      }
      let out = logic
        .process_input(&mut state, LogicInput::TimeStep {
          delta_time: std::time::Duration::from_millis(33),
        })
        .await
        .unwrap();
      let mut frame = None;
      for targeted in &out.ops {
        for op in &targeted.ops {
          if let GowOp::World(f) = op
            && f.you.map(|y| y.seat) == state.seat_of(1)
          {
            frame = Some((**f).clone());
          }
        }
      }
      let mut seen = frame.expect("a frame").seen();
      seen.sort_by_key(|s| s.seat);
      seen
    }

    let absolute = world(Precision::Absolute).await;
    let relative = world(Precision::CellRelative).await;
    let graded = world(Precision::Graded).await;
    assert!(!absolute.is_empty(), "the zone has to describe somebody");
    assert_eq!(absolute.len(), relative.len(), "cell-relative lost or gained bodies");
    assert_eq!(absolute.len(), graded.len(), "graded lost or gained bodies");

    // The coarse width is the loosest any of them may be, and it is still
    // well inside a pixel at the distance it is used.
    let coarse_step = (crate::zone::CELL * 2.0) / ((1u32 << crate::pack::GRADED_COARSE_BITS) - 1) as f32;
    for ((a, r), g) in absolute.iter().zip(&relative).zip(&graded) {
      assert_eq!(a.seat, r.seat);
      assert_eq!(a.seat, g.seat);
      assert!((a.at.0 - r.at.0).abs() <= coarse_step, "seat {} moved", a.seat);
      assert!((a.at.0 - g.at.0).abs() <= coarse_step, "seat {} moved under graded", a.seat);
      assert!((a.at.2 - g.at.2).abs() <= coarse_step);
    }
  }

  #[tokio::test]
  async fn a_cell_op_goes_to_everyone_watching_that_cell_and_nobody_else() {
    // What `MessageTarget::Agents` buys, and the property that makes it worth
    // a protocol change: one encode reaches every viewer of a cell. If each
    // op named one recipient this would be a per-client frame with extra steps.
    let logic = GowLogic::new();
    let mut state = GowState::new();
    state.delivery = Delivery::Cells;
    let near = seat_for(&mut state, 1);
    let also = seat_for(&mut state, 2);
    let far = seat_for(&mut state, 3);
    state.zone.place(far, (500.0, 0.0, 500.0));
    let _ = (near, also);

    let out = logic
      .process_input(&mut state, LogicInput::TimeStep {
        delta_time: std::time::Duration::from_millis(33),
      })
      .await
      .unwrap();

    let shared = out
      .ops
      .iter()
      .filter(|t| t.ops.iter().any(|op| matches!(op, GowOp::Cell(_))))
      .find(|t| matches!(&t.target, plaza::session::MessageTarget::Agents(ids) if ids.len() > 1))
      .expect("a cell two neighbours can both see is addressed to both at once");
    let plaza::session::MessageTarget::Agents(ids) = &shared.target else {
      unreachable!()
    };
    assert!(ids.contains(&1) && ids.contains(&2), "both neighbours hear it: {ids:?}");
    assert!(!ids.contains(&3), "and the seat across the zone does not");
  }

  #[test]
  fn a_body_past_the_index_rides_a_border_cell_into_a_frame() {
    // The zone_scale defect, pinned. `GridQuantizer` clamps anything outside
    // its origin into the boundary cells, and a cell is published whole, so a
    // body five hundred units away arrives in the frame of anyone standing in
    // the corner. The per-client build's exact distance test hid this and
    // charged only query waste for it; publishing per cell puts it on the wire.
    let corner = -crate::terrain::EDGE + 1.0;
    let mut state = GowState::new();
    let viewer = seated(&mut state, 1);
    let far = seated(&mut state, 2);
    state.zone.place(viewer, (corner, 0.0, corner));
    state.zone.place(far, (-500.0, 0.0, -500.0));

    let seen = built(&mut state, viewer, 0).seen();
    assert!(
      seen.iter().any(|c| c.seat == far),
      "an index smaller than its world leaks the pile it clamped"
    );

    // Sized to the world it holds, the same arrangement does not.
    let mut state = GowState::spanning(crate::state::MAX_CHARACTERS, 600.0);
    let viewer = seated(&mut state, 1);
    let far = seated(&mut state, 2);
    state.zone.place(viewer, (corner, 0.0, corner));
    state.zone.place(far, (-500.0, 0.0, -500.0));

    let seen = built(&mut state, viewer, 0).seen();
    assert!(
      !seen.iter().any(|c| c.seat == far),
      "a body 550 units away is in the frame of an index that reaches it"
    );
  }

  #[test]
  fn a_frame_says_why_each_character_is_in_it() {
    // The distinction the whole example rests on: a client that cannot tell
    // these apart cannot draw a party frame for somebody out of view.
    let mut state = GowState::new();
    let a = seated(&mut state, 1);
    let b = seated(&mut state, 2);
    let far = seated(&mut state, 3);
    state.zone.place(far, (500.0, 0.0, 500.0));
    state.zone.parties.join(a, far);

    let frame = built(&mut state, a, 0);
    let seen = frame.seen();
    let why = |s: Seat| seen.iter().find(|c| c.seat == s).map(|c| c.because);

    assert_eq!(why(a), Some(Because::Near), "yourself is not your own party member");
    assert_eq!(why(b), Some(Because::Near), "a neighbour you did not choose");
    assert_eq!(why(far), Some(Because::Subscribed), "and one you did, across the zone");
  }

  #[test]
  fn a_party_member_standing_next_to_you_is_one_entry_labelled_both() {
    let mut state = GowState::new();
    let a = seated(&mut state, 1);
    let b = seated(&mut state, 2);
    state.zone.parties.join(a, b);

    let frame = built(&mut state, a, 0);
    assert_eq!(frame.seen().len(), 2, "nobody is listed twice");
    let bodies = frame.seen();
    let seen = bodies.iter().find(|c| c.seat == b).unwrap();
    assert_eq!(seen.because, Because::BothOfThose);
    assert!(seen.because.is_near() && seen.because.is_subscribed());
  }

  #[test]
  fn a_landing_across_the_zone_is_not_news() {
    // Sending it would describe something the client has no character for,
    // which is how a client ends up playing an animation on nothing.
    let mut state = GowState::new();
    let a = seated(&mut state, 1);
    let far = seated(&mut state, 2);
    state.zone.place(far, (500.0, 0.0, 500.0));
    state.landed = vec![crate::protocol::Landed { seat: far, ability: 0, victim: None }];

    let frame = built(&mut state, a, 0);
    assert!(frame.landed.is_empty());

    state.zone.place(far, spawn_at(far));
    let frame = built(&mut state, a, 0);
    assert_eq!(frame.landed.len(), 1, "and one beside you is");
    assert_eq!(frame.landed[0].seat, far);
  }

  #[tokio::test]
  async fn a_refused_claim_answers_the_claimant_and_nobody_else() {
    // The only op in this example that goes back to one client, and the reason
    // it is not a correction: an honest client never sees one.
    let logic = GowLogic::new();
    let mut state = GowState::new();
    let seat = seated(&mut state, 1);

    let mut ctx = Ctx::new();
    apply(&mut state, 1, seat, GowOp::Moved { at: (900.0, 0.0, 0.0), yaw: 0.0 }, &mut ctx);
    let ops = ctx.into_ops();
    assert_eq!(ops.len(), 1);
    assert!(matches!(ops[0].ops[0], GowOp::Refused { .. }));
    assert_eq!(state.zone.refusals, 1);

    let _ = logic;
  }

  #[tokio::test]
  async fn the_zone_reports_on_a_tick_boundary_and_resets_only_the_window() {
    // A counter that describes a window and one that describes a session read
    // differently, and resetting both together is how a log starts lying
    // slowly: the totals would restart every ten seconds while claiming to be
    // totals.
    let logic = GowLogic::new();
    let mut state = GowState::new();
    seated(&mut state, 1);
    state.zone.landed = 7;
    state.zone.refusals = 3;

    state.zone.examined = 999_999;
    state.tick = REPORT_EVERY - 1;
    logic
      .process_input(&mut state, LogicInput::TimeStep {
        delta_time: std::time::Duration::from_millis(33),
      })
      .await
      .unwrap();

    assert!(
      state.zone.examined < 999_999,
      "the window did not reset: {}",
      state.zone.examined
    );
    assert_eq!(state.zone.landed, 7, "and the totals do not");
    assert_eq!(state.zone.refusals, 3);
  }

  #[tokio::test]
  async fn a_respawn_tells_the_client_it_was_moved() {
    // The client owns its own position, so a respawn is the one time a
    // position travels downward. Without the counter it stands where it died,
    // sending claims the server refuses for the rest of the session.
    use crate::zone::DOWN_MS;
    let mut state = GowState::new();
    let seat = seated(&mut state, 1);
    let before = you_of(&state, seat, 0).unwrap();

    state.zone.characters.get_mut(&seat).unwrap().alive = false;
    state.zone.characters.get_mut(&seat).unwrap().health = 0;
    state.zone.characters.get_mut(&seat).unwrap().up_at = DOWN_MS;
    state.zone.advance(DOWN_MS + 1);

    let after = you_of(&state, seat, state.zone.now_ms).unwrap();
    assert!(after.spawn > before.spawn, "the client is never told it moved");
    assert_eq!(
      after.at,
      state.zone.characters[&seat].tracked.at,
      "and it has to be told where"
    );
    assert!(after.up_in_ms.is_none(), "it should be back up");
  }

  #[test]
  fn a_body_stays_in_the_frame_while_it_falls_and_then_goes() {
    // A beast that vanished the instant it died read as a rendering fault
    // rather than as a death, because nothing on screen ever fell over. The
    // window is long enough to play the fall and short enough that the two
    // relevance channels still come apart afterwards.
    use crate::zone::{CORPSE_MS, DOWN_MS};
    let mut state = GowState::new();
    let a = seated(&mut state, 1);
    let far = seated(&mut state, 2);
    state.zone.admit_beast(9, spawn_at(a));

    state.zone.characters.get_mut(&9).unwrap().alive = false;
    state.zone.characters.get_mut(&9).unwrap().health = 0;
    state.zone.characters.get_mut(&9).unwrap().up_at = state.zone.now_ms + DOWN_MS;

    let now = state.zone.now_ms;
    let frame = built(&mut state, a, now);
    assert!(
      frame.seen().iter().any(|c| c.seat == 9),
      "the body was gone before it could fall"
    );

    state.zone.advance(CORPSE_MS + 100);
    let now = state.zone.now_ms;
    let frame = built(&mut state, a, now);
    assert!(
      !frame.seen().iter().any(|c| c.seat == 9),
      "the body never left"
    );

    // And a subscribed one is still there, which is the distinction the
    // window was kept short to preserve.
    state.zone.parties.join(a, far);
    state.zone.characters.get_mut(&far).unwrap().alive = false;
    state.zone.characters.get_mut(&far).unwrap().health = 0;
    state.zone.characters.get_mut(&far).unwrap().up_at = now + DOWN_MS;
    state.zone.advance(CORPSE_MS + 100);
    let now = state.zone.now_ms;
    let frame = built(&mut state, a, now);
    let bodies = frame.seen();
    let entry = bodies.iter().find(|c| c.seat == far);
    assert!(entry.is_some(), "a downed party member left the party frame");
    assert_eq!(entry.unwrap().health, 0);
  }

  #[tokio::test]
  async fn a_zone_with_bots_seats_them_on_the_first_tick() {
    // The complaint that started this: a player joined a tower with nobody in
    // it, so every key was dead and the panel reported on an audience of zero.
    let logic = GowLogic::new().with_bots(24);
    let mut state = GowState::new();
    logic
      .process_input(&mut state, LogicInput::TimeStep {
        delta_time: std::time::Duration::from_millis(33),
      })
      .await
      .unwrap();

    assert_eq!(state.bots.len(), 24, "no adventurers were seated");
    let beasts = state.zone.characters.values().filter(|c| c.is_beast()).count();
    assert_eq!(beasts, BEASTS, "no beasts were seated");
    assert!(
      state.zone.parties.of(0).count() > 0,
      "nobody was partied, so the second channel is empty before a player acts"
    );
  }

  #[tokio::test]
  async fn a_bare_zone_stays_bare() {
    // The measurements and every other test in here want an empty zone, so
    // seating content must be something a caller asks for.
    let logic = GowLogic::new();
    let mut state = GowState::new();
    logic
      .process_input(&mut state, LogicInput::TimeStep {
        delta_time: std::time::Duration::from_millis(33),
      })
      .await
      .unwrap();
    assert!(state.zone.characters.is_empty());
  }

  #[tokio::test]
  async fn ops_from_a_departed_seat_are_dropped_rather_than_erroring() {
    // A packet that crossed a departure is normal, not a fault: refusing it
    // loudly would kill connections for a race the network guarantees.
    let logic = GowLogic::new();
    let mut state = GowState::new();
    let out = logic
      .process_input(
        &mut state,
        LogicInput::AgentOps {
          source: Agent::new_human(7u32),
          ops: vec![GowOp::Cast { ability: 0, cast_ms: 1500 }],
        },
      )
      .await
      .expect("a stale packet is not an error");
    assert!(out.ops.is_empty());
  }
}
