//! The tick, which is mostly a send.
//!
//! There is no simulation step here worth the name: nobody's position is
//! computed, because the clients own those, and the only thing with a clock is
//! a cast bar. What the tick actually does is answer, once per client, the
//! question this example exists to ask: **who are you told about, and why**.
//!
//! That per-client shape is the cost. One frame cannot be built and broadcast,
//! because two characters standing in different corners of the zone have
//! nothing in common, and a party makes even neighbours differ.

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
use crate::protocol::{frame_to_ms, Because, Frame, GowOp, PlayerId, Seen, You, TICK_HZ};
use crate::relevance::Seat;
use crate::state::{den_at, spawn_at, GowState, MAX_CHARACTERS};

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

    let room = MAX_CHARACTERS.saturating_sub(self.beasts);
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
      let id = PlayerId::MAX - (MAX_CHARACTERS + i) as PlayerId;
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
          state.zone.authority = dial.lock().authority;
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
    GowOp::World(_) | GowOp::Seated { .. } | GowOp::Refused { .. } => {}
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

  for (player, seat) in players {
    let frame = frame_for(state, seat, now);
    ctx
      .ops_q()
      .push(TargetedOp::new_system_to(player, vec![GowOp::World(Box::new(frame))]));
  }
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

/// One client's view, which is the two channels unioned and then labelled.
fn frame_for(state: &mut GowState, seat: Seat, now: Ms) -> Frame {
  let tick = state.tick;
  let landed = state.landed.clone();
  let authority = state.zone.authority;
  let you = you_of(state, seat, now);
  state.with_scratch(|zone, scratch| {
    let audience = zone.audience_for(seat, scratch);
    let near: std::collections::HashSet<Seat> = scratch.iter().copied().collect();
    let characters = audience
      .seats
      .iter()
      .filter_map(|s| {
        let character = zone.characters.get(s)?;
        // A body falls where everyone can see it, and then it is gone from
        // them and still in the party frame of anyone subscribed. Dropping it
        // the instant it died is what made beasts vanish in mid-air.
        if !character.alive
          && !character.still_falling(now)
          && !zone.parties.of(seat).any(|m| m == *s)
          && *s != seat
        {
          return None;
        }
        let subscribed = *s != seat && zone.parties.of(seat).any(|m| m == *s);
        let because = match (near.contains(s), subscribed) {
          (true, true) => Because::BothOfThose,
          (true, false) => Because::Near,
          (false, _) => Because::Subscribed,
        };
        Some(Seen {
          seat: *s,
          at: character.tracked.at,
          health: character.health,
          max_health: character.max_health,
          yaw: character.yaw,
          kind: character.kind,
          because,
          casting_ms: character
            .casting
            .map(|cast| cast.lands_at.saturating_sub(now) as u32),
        })
      })
      .collect();
    Frame {
      tick,
      you,
      authority,
      characters,
      // Only the ones this client can see. A landing across the zone is not
      // news, and sending it would be describing something the client has no
      // character for.
      landed: landed.iter().copied().filter(|s| near.contains(s)).collect(),
    }
  })
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
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn seated(state: &mut GowState, player: PlayerId) -> Seat {
    let Admission::Seated { seat, .. } = state.roster.admit(player) else {
      panic!("no seat");
    };
    let seat = seat as Seat;
    state.zone.admit(seat, spawn_at(seat));
    seat
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

    let frame = frame_for(&mut state, a, 0);
    let why = |s: Seat| frame.characters.iter().find(|c| c.seat == s).map(|c| c.because);

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

    let frame = frame_for(&mut state, a, 0);
    assert_eq!(frame.characters.len(), 2, "nobody is listed twice");
    let seen = frame.characters.iter().find(|c| c.seat == b).unwrap();
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
    state.landed = vec![far];

    let frame = frame_for(&mut state, a, 0);
    assert!(frame.landed.is_empty());

    state.zone.place(far, spawn_at(far));
    let frame = frame_for(&mut state, a, 0);
    assert_eq!(frame.landed, vec![far], "and one beside you is");
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
    let frame = frame_for(&mut state, a, now);
    assert!(
      frame.characters.iter().any(|c| c.seat == 9),
      "the body was gone before it could fall"
    );

    state.zone.advance(CORPSE_MS + 100);
    let now = state.zone.now_ms;
    let frame = frame_for(&mut state, a, now);
    assert!(
      !frame.characters.iter().any(|c| c.seat == 9),
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
    let frame = frame_for(&mut state, a, now);
    let entry = frame.characters.iter().find(|c| c.seat == far);
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
