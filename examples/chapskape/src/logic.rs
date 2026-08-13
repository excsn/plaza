//! The tick, and the frame it produces.
//!
//! The host wakes twenty times a second and the world moves once or twice in
//! that, which is deliberate: a game tick is a budget drawn down rather than a
//! wake-up answered, so its length is a dial rather than a constant. Turning it
//! from 600ms to 50ms is the experiment, and what it costs is the finding.
//!
//! A frame is built per client, because two people standing in different
//! corners of a map have nothing in common. What is in one splits three ways:
//!
//! - **Actors**, repeated every tick, because everyone moves and a state has to
//!   be repeated to stay true.
//! - **Props**, sent once each way under [`Relevance::OnChange`], because
//!   almost nothing ever happens to them and an absolute tick is the same
//!   answer every time it is asked.
//! - **Events**, sent exactly once, because a transcript is not a state and no
//!   later frame will mention a blow again.

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::common::fsm::{FsmContext as _, OpsQueue};
use plaza::error::StateLogicError;
use plaza::session::TargetedOp;
use plaza::state_logic::{LogicInput, LogicOutput, StateLogic};
use plaza_server_utils::{Admission, Departure};
use tracing::info;

use crate::controls::Dial;
use crate::pack::SLOTS;
use crate::path::Goal;
use crate::protocol::{
  Frame, Happened, PlayerId, Private, Queued, Seat, Seen, SkapeOp, Tile, You, DRIVER_MS,
};
use crate::state::SkapeState;
use crate::world;
use crate::zone::VIEW;

type Ctx = OpsQueue<SkapeOp, PlayerId>;

/// How often the world says what it has been doing, in game ticks.
pub const REPORT_EVERY: u64 = 50;

/// Hens, which are there to be a first fight rather than a threat.
pub const HENS: usize = 60;
/// Brutes, which are the reason a level matters.
pub const BRUTES: usize = 46;

/// The most game ticks one wake-up may run.
///
/// A host that stalled and came back owing two seconds must not spend them all
/// in one frame: the world would jump and every client would watch it happen.
const CATCH_UP: usize = 3;

#[derive(Default)]
pub struct SkapeLogic {
  clock: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
  dial: Option<Dial>,
  bots: usize,
  foes: usize,
}

impl std::fmt::Debug for SkapeLogic {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("SkapeLogic")
  }
}

impl SkapeLogic {
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

  /// Seats the world's own. Zero is a bare map, which is what the measurements
  /// and most of the tests want.
  pub fn with_bots(mut self, bots: usize) -> Self {
    self.bots = bots;
    self.foes = if bots == 0 { 0 } else { HENS + BRUTES };
    self
  }

  pub fn with_foes(mut self, foes: usize) -> Self {
    self.foes = foes;
    self
  }

  fn populate(&self, state: &mut SkapeState) {
    if state.populated {
      return;
    }
    state.populated = true;

    let mut crew = std::mem::take(&mut state.bots);
    for index in 0..self.bots {
      let id = PlayerId::MAX - index as PlayerId;
      let Admission::Seated { seat, .. } = state.roster.admit(id) else {
        break;
      };
      let seat = seat as Seat;
      let angle = index as f32 * 2.399_963_2;
      // Tighter than a plain spiral would be. A world this wide spreads a
      // population out until a view of it is empty, and an empty view is the
      // one thing this example cannot afford: it is what made gow_3d's bugs
      // invisible.
      let radius = 4.0 + (index as f32).sqrt() * 3.5;
      let hint = Tile::new(
        world::SIZE / 2 + (angle.cos() * radius) as i16,
        world::SIZE / 2 + (angle.sin() * radius) as i16,
      );
      state.zone.admit(seat, world::footing_near(hint), crate::protocol::Look::Person);
      crew.take_seat(seat, index);
    }
    state.bots = crew;

    let mut foes = Vec::new();
    for index in 0..self.foes {
      let id = PlayerId::MAX - (1000 + index) as PlayerId;
      let Admission::Seated { seat, .. } = state.roster.admit(id) else {
        break;
      };
      foes.push(seat as Seat);
    }
    crate::bots::stock(&mut state.zone, foes.into_iter(), HENS.min(self.foes));
  }
}

#[async_trait]
impl StateLogic<SkapeOp, PlayerId, SkapeState> for SkapeLogic {
  async fn process_input(
    &self,
    state: &mut SkapeState,
    input: LogicInput<SkapeOp, PlayerId>,
  ) -> Result<LogicOutput<SkapeOp, PlayerId>, StateLogicError> {
    let mut ctx = Ctx::new();

    match input {
      LogicInput::AgentJoined { agent } => seat_player(state, &agent, &mut ctx),
      LogicInput::AgentLeft { agent_id } => depart(state, agent_id),
      LogicInput::AgentOps { source, ops } => {
        let Some(player) = source.id_cloned() else {
          return Err(StateLogicError::InvalidOperation("ops from an unidentified agent".into()));
        };
        // A player whose seat has gone is not an error, it is a packet that
        // crossed a departure.
        if let Some(seat) = state.seat_of(player) {
          for op in ops {
            apply(state, seat, op);
          }
        }
      }
      LogicInput::TimeStep { delta_time } => {
        // Read once a wake rather than once a frame, so a change lands on a
        // tick boundary and cannot split one.
        if let Some(dial) = &self.dial {
          let dial = dial.lock();
          state.mode = dial.objects;
          state.tick_ms = dial.tick_ms.max(DRIVER_MS);
        }
        self.populate(state);

        let elapsed = delta_time.as_millis().max(1) as u64;
        state.now_ms += elapsed;
        state.owed_ms += elapsed;
        let mut ran = 0;
        while state.owed_ms >= state.tick_ms && ran < CATCH_UP {
          state.owed_ms -= state.tick_ms;
          step_once(state, &mut ctx);
          ran += 1;
        }
        if ran == CATCH_UP {
          // Whatever is still owed after a catch-up is time the world will
          // never have. Keeping it would make the next wake worse.
          state.owed_ms = 0;
        }
      }
    }

    if let Some(clock) = &self.clock {
      clock.store(state.now_ms, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(LogicOutput {
      ops: ctx.into_ops(),
      ..Default::default()
    })
  }
}

fn seat_player(state: &mut SkapeState, agent: &Agent<PlayerId>, ctx: &mut Ctx) {
  let Some(player) = agent.id_cloned() else {
    return;
  };
  let Admission::Seated { seat, .. } = state.roster.admit(player) else {
    return;
  };
  let seat = seat as Seat;
  let tile = world::footing_near(world::the_green());
  state.agents.insert(player, agent.clone());
  state.zone.admit(seat, tile, crate::protocol::Look::Person);
  ctx
    .ops_q()
    .push(TargetedOp::new_system_to(player, vec![SkapeOp::Seated { seat, tile }]));
}

fn depart(state: &mut SkapeState, player: PlayerId) {
  state.agents.remove(&player);
  if let Departure::Freed { seat } = state.roster.depart(&player) {
    state.zone.remove(seat as Seat);
    state.forget(seat as Seat);
  }
}

fn apply(state: &mut SkapeState, seat: Seat, op: SkapeOp) {
  match op {
    SkapeOp::WalkTo { tile } => state.zone.walk_to(seat, tile),
    SkapeOp::Interact { object } => {
      let what = if object >= world::FIRE_BASE {
        Some(Queued::Cook { fire: object })
      } else {
        world::prop_at(world::prop_tile(object)).map(|prop| match prop {
          world::Prop::Tree | world::Prop::Oak => Queued::Chop { object },
          world::Prop::Rock | world::Prop::Vein => Queued::Mine { object },
          world::Prop::Fish => Queued::Fish { object },
        })
      };
      match what {
        Some(what) => state.zone.queue(seat, what),
        None => state.zone.walk_to(seat, world::prop_tile(object)),
      }
    }
    SkapeOp::Attack { seat: other } => state.zone.queue(seat, Queued::Fight { seat: other }),
    SkapeOp::Take { ground } => state.zone.queue(seat, Queued::Take { ground }),
    SkapeOp::Drop { slot } => state.zone.drop_slot(seat, slot),
    SkapeOp::Use { slot } => state.zone.use_slot(seat, slot),
    SkapeOp::Run { on } => state.zone.set_running(seat, on),
    SkapeOp::Cancel => state.zone.cancel(seat),
    // Server-to-client ops arriving from a client are noise, not a protocol
    // error worth killing a connection over.
    SkapeOp::World(_) | SkapeOp::Seated { .. } => {}
  }
}

fn step_once(state: &mut SkapeState, ctx: &mut Ctx) {
  let mut bots = std::mem::take(&mut state.bots);
  bots.steer(&mut state.zone);
  state.bots = bots;
  state.zone.advance();

  if state.zone.tick.is_multiple_of(REPORT_EVERY) {
    report(state);
  }

  let players: Vec<(PlayerId, Seat)> = state
    .agents
    .keys()
    .filter_map(|p| state.seat_of(*p).map(|s| (*p, s)))
    .collect();

  for (player, seat) in players {
    let frame = frame_for(state, seat);
    state.frames_sent += 1;
    ctx
      .ops_q()
      .push(TargetedOp::new_system_to(player, vec![SkapeOp::World(Box::new(frame))]));
  }
}

/// Says what the world has been doing, for a server nobody is watching a panel
/// for.
fn report(state: &mut SkapeState) {
  let saved = if state.object_entries_repeated == 0 {
    0.0
  } else {
    (1.0 - state.object_entries as f64 / state.object_entries_repeated as f64) * 100.0
  };
  info!(
    tick = state.zone.tick,
    tick_ms = state.tick_ms,
    actors = state.zone.actors.len(),
    watching = state.agents.len(),
    gathered = state.zone.gathered,
    blows = state.zone.blows,
    felled = state.zone.falls,
    props_out = state.zone.depleted.len(),
    on_the_ground = state.zone.ground.len(),
    routes = state.zone.routes_found,
    squares_searched = state.zone.squares_searched,
    objects = state.mode.label(),
    object_entries_saved = format!("{saved:.0}%"),
    "world"
  );
}

/// One client's view of the world.
fn frame_for(state: &mut SkapeState, seat: Seat) -> Frame {
  let tick = state.zone.tick;
  let tick_ms = state.tick_ms as u16;
  let mode = state.mode;
  let Some(actor) = state.zone.actors.get(&seat) else {
    return Frame {
      tick,
      tick_ms,
      you: None,
      actors: Vec::new(),
      objects: Vec::new(),
      fires: Vec::new(),
      ground: Vec::new(),
      events: Vec::new(),
      mode,
    };
  };
  let middle = actor.tile;

  // Everyone but the viewer. A client never appears in its own audience, and
  // what it needs to know about itself is not a subset of what it is told about
  // anybody else, so it travels in `You` instead.
  let mut actors: Vec<Seen> = state
    .zone
    .actors
    .iter()
    .filter(|(other, _)| **other != seat)
    .filter(|(_, them)| middle.steps_to(them.tile) <= VIEW)
    .map(|(other, them)| Seen {
      seat: *other,
      tile: them.tile,
      look: them.look,
      doing: them.doing,
      health: them.health,
      max_health: them.max_health,
      facing: them.facing,
    })
    .collect();
  actors.sort_unstable_by_key(|seen| seen.seat);

  let events: Vec<Happened> = state
    .zone
    .events
    .iter()
    .filter(|(at, _)| middle.steps_to(*at) <= VIEW)
    .map(|(_, what)| *what)
    .collect();

  let fires = state.zone.fires_in_view(middle);
  let ground = state.zone.ground_in_view(middle, seat);
  let you = you_of(state, seat);
  let objects = state.objects_for(seat, middle);
  if you.as_ref().is_some_and(|you| you.private.is_some()) {
    state.private_sent += 1;
  }

  Frame {
    tick,
    tick_ms,
    you: you.map(Box::new),
    actors,
    objects,
    fires,
    ground,
    events,
    mode,
  }
}

/// What a player is told about themselves, including the one stream nobody else
/// has any part of.
fn you_of(state: &mut SkapeState, seat: Seat) -> Option<You> {
  let tick = state.zone.tick;
  let actor = state.zone.actors.get_mut(&seat)?;
  // Sent only when it moved. A pack that has not changed is a pack the client
  // already has, and standing in a field is most of a session.
  let private = actor.private_moved.then(|| Private {
    pack: actor.pack.as_vec(),
    xp: actor.xp.to_vec(),
  });
  actor.private_moved = false;
  let refused = actor.refused.take();
  // Taken rather than copied: a transcript said twice is a level announced
  // twice, and the zone clears these on the next tick anyway.
  let happened = std::mem::take(&mut actor.told);
  Some(You {
    seat,
    tile: actor.tile,
    health: actor.health,
    max_health: actor.max_health,
    doing: actor.doing,
    facing: actor.facing,
    queued: actor.queued,
    running: actor.running,
    up_in: (!actor.alive()).then(|| actor.up_at.saturating_sub(tick).min(u16::MAX as u64) as u16),
    private,
    happened,
    refused,
    spawn: actor.spawns,
  })
}

/// A destination expanded the way the client would have expanded it.
///
/// Here so a test can ask the question the whole design turns on without
/// standing a server up.
pub fn route_as_a_client_would(from: Tile, goal: Goal) -> Vec<Tile> {
  crate::path::Pathfinder::new().route(from, goal)
}

const _: () = assert!(SLOTS == 28);

#[cfg(test)]
mod tests {
  use super::*;
  use crate::protocol::{Item, Look, Refusal, Relevance};

  fn seated(state: &mut SkapeState, player: PlayerId) -> Seat {
    let Admission::Seated { seat, .. } = state.roster.admit(player) else {
      panic!("no seat");
    };
    let seat = seat as Seat;
    state.agents.insert(player, Agent::new_human(player));
    state
      .zone
      .admit(seat, world::footing_near(world::the_green()), Look::Person);
    seat
  }

  #[tokio::test]
  async fn a_client_is_never_in_its_own_audience() {
    // Which is why `You` exists at all. gow_3d shipped the other way round and
    // every key press was silent.
    let mut state = SkapeState::new();
    let me = seated(&mut state, 1);
    let neighbour = seated(&mut state, 2);

    let frame = frame_for(&mut state, me);
    assert!(!frame.actors.iter().any(|a| a.seat == me), "the viewer is in its own list");
    assert!(frame.actors.iter().any(|a| a.seat == neighbour));
    let you = frame.you.expect("no You block");
    assert_eq!(you.seat, me);
    assert_eq!(you.tile, state.zone.actors[&me].tile);
  }

  #[tokio::test]
  async fn the_private_stream_is_sent_when_it_moves_and_not_otherwise() {
    // The measurement that makes a pack cheap: a client standing in a field
    // pays nothing for twenty-eight squares it already knows about.
    let mut state = SkapeState::new();
    let me = seated(&mut state, 1);

    assert!(frame_for(&mut state, me).you.unwrap().private.is_some(), "never sent at all");
    assert!(frame_for(&mut state, me).you.unwrap().private.is_none(), "sent again unchanged");

    state.zone.actors.get_mut(&me).unwrap().pack.add(Item::Logs);
    state.zone.actors.get_mut(&me).unwrap().private_moved = true;
    let private = frame_for(&mut state, me).you.unwrap().private.expect("a change was silent");
    assert_eq!(private.pack.len(), SLOTS);
    assert_eq!(private.pack[0], Some(Item::Logs));
    assert!(frame_for(&mut state, me).you.unwrap().private.is_none());
  }

  #[tokio::test]
  async fn a_refusal_reaches_the_one_who_asked_and_is_said_once() {
    let mut state = SkapeState::new();
    let me = seated(&mut state, 1);
    apply(&mut state, me, SkapeOp::Drop { slot: 0 });

    assert_eq!(frame_for(&mut state, me).you.unwrap().refused, Some(Refusal::PackEmpty));
    assert_eq!(frame_for(&mut state, me).you.unwrap().refused, None, "said twice");
  }

  #[tokio::test]
  async fn an_event_across_the_map_is_not_news() {
    // Sending it would describe something the client has no body for, which is
    // how a client ends up playing an animation on nothing.
    let mut state = SkapeState::new();
    let me = seated(&mut state, 1);
    let here = state.zone.actors[&me].tile;
    state.zone.events.push((
      Tile::new(here.x + VIEW as i16 + 5, here.y),
      Happened::Fell { seat: 99 },
    ));
    assert!(frame_for(&mut state, me).events.is_empty());

    state.zone.events.push((here, Happened::Fell { seat: 98 }));
    assert_eq!(frame_for(&mut state, me).events.len(), 1, "and one beside you is");
  }

  #[tokio::test]
  async fn walking_costs_one_op_and_several_seconds() {
    // The headline claim, asserted rather than asserted about. One op moves a
    // body for as long as the route is long, and nothing comes back.
    let logic = SkapeLogic::new();
    let mut state = SkapeState::new();
    let me = seated(&mut state, 1);
    let from = state.zone.actors[&me].tile;
    let to = world::footing_near(Tile::new(from.x + 15, from.y + 10));

    apply(&mut state, me, SkapeOp::WalkTo { tile: to });
    let route = state.zone.actors[&me].route.len();
    assert!(route >= 10, "one op bought only {route} squares");

    let mut ctx = Ctx::new();
    for _ in 0..route {
      step_once(&mut state, &mut ctx);
    }
    assert_eq!(state.zone.actors[&me].tile, to, "one op did not finish the journey");
    // Everything the server said back was an ordinary frame. There is no
    // correction op in this protocol, because there is nothing to correct.
    let _ = logic;
  }

  #[tokio::test]
  async fn the_client_and_the_server_expand_the_same_click_the_same_way() {
    // The property the design rests on, checked across the seam rather than
    // inside the pathfinder: the server's route and the one a client would have
    // drawn are the same squares in the same order.
    let mut state = SkapeState::new();
    let me = seated(&mut state, 1);
    for step in 1..40i16 {
      let from = state.zone.actors[&me].tile;
      let to = world::footing_near(Tile::new(from.x + step, from.y + step / 2));
      apply(&mut state, me, SkapeOp::WalkTo { tile: to });
      let server: Vec<Tile> = state.zone.actors[&me].route.iter().copied().collect();
      let client = route_as_a_client_would(from, Goal::On(to));
      assert_eq!(server, client, "the two ends disagreed walking to {to:?}");
      state.zone.cancel(me);
      state.zone.actors.get_mut(&me).unwrap().tile = to;
    }
  }

  #[tokio::test]
  async fn a_bare_world_stays_bare() {
    let logic = SkapeLogic::new();
    let mut state = SkapeState::new();
    logic
      .process_input(&mut state, LogicInput::TimeStep {
        delta_time: std::time::Duration::from_millis(DRIVER_MS),
      })
      .await
      .unwrap();
    assert!(state.zone.actors.is_empty());
  }

  #[tokio::test]
  async fn a_world_with_bots_is_inhabited_before_anybody_arrives() {
    let logic = SkapeLogic::new().with_bots(12);
    let mut state = SkapeState::new();
    logic
      .process_input(&mut state, LogicInput::TimeStep {
        delta_time: std::time::Duration::from_millis(DRIVER_MS),
      })
      .await
      .unwrap();
    assert_eq!(state.bots.len(), 12, "nobody was seated");
    let foes = state.zone.actors.values().filter(|a| a.look.is_foe()).count();
    assert_eq!(foes, HENS + BRUTES, "nothing to fight");
  }

  #[tokio::test]
  async fn somebody_joining_later_still_arrives_somewhere_inhabited() {
    // The complaint that started gow_3d, guarded against here rather than
    // discovered by playing: the world's own wander, and a world whose people
    // have all wandered off is a world where every frame is empty and no key
    // looks like it works.
    let logic = SkapeLogic::new().with_bots(24);
    let mut state = SkapeState::new();
    for _ in 0..(200 * crate::protocol::TICK_MS / DRIVER_MS) {
      logic
        .process_input(&mut state, LogicInput::TimeStep {
          delta_time: std::time::Duration::from_millis(DRIVER_MS),
        })
        .await
        .unwrap();
    }
    assert_eq!(state.zone.tick, 200);
    let mut ctx = Ctx::new();
    seat_player(&mut state, &Agent::new_human(1u32), &mut ctx);
    let me = state.seat_of(1).unwrap();
    let frame = frame_for(&mut state, me);
    println!(
      "\n  after 200 ticks a joiner sees {} bodies, {} things on the ground\n",
      frame.actors.len(),
      frame.ground.len()
    );
    assert!(
      frame.actors.len() >= 10,
      "a joiner arrived to {} bodies, which is an empty world",
      frame.actors.len()
    );
  }

  #[tokio::test]
  async fn the_world_moves_once_a_tick_however_often_the_host_wakes() {
    // Twelve wake-ups at fifty milliseconds is one game tick at six hundred,
    // which is what makes the tick length a dial rather than a constant.
    let logic = SkapeLogic::new();
    let mut state = SkapeState::new();
    for _ in 0..11 {
      logic
        .process_input(&mut state, LogicInput::TimeStep {
          delta_time: std::time::Duration::from_millis(DRIVER_MS),
        })
        .await
        .unwrap();
    }
    assert_eq!(state.zone.tick, 0, "the world moved early");
    logic
      .process_input(&mut state, LogicInput::TimeStep {
        delta_time: std::time::Duration::from_millis(DRIVER_MS),
      })
      .await
      .unwrap();
    assert_eq!(state.zone.tick, 1);
  }

  #[tokio::test]
  async fn a_stalled_host_does_not_spend_a_lost_second_all_at_once() {
    let logic = SkapeLogic::new();
    let mut state = SkapeState::new();
    logic
      .process_input(&mut state, LogicInput::TimeStep {
        delta_time: std::time::Duration::from_millis(6000),
      })
      .await
      .unwrap();
    assert_eq!(state.zone.tick, CATCH_UP as u64, "the world jumped {} ticks", state.zone.tick);
    assert_eq!(state.owed_ms, 0, "and it is still owed time it will never have");
  }

  #[tokio::test]
  async fn ops_from_a_departed_seat_are_dropped_rather_than_erroring() {
    let logic = SkapeLogic::new();
    let mut state = SkapeState::new();
    let out = logic
      .process_input(&mut state, LogicInput::AgentOps {
        source: Agent::new_human(7u32),
        ops: vec![SkapeOp::Cancel],
      })
      .await
      .expect("a stale packet is not an error");
    assert!(out.ops.is_empty());
  }

  #[tokio::test]
  async fn what_a_frame_carries_when_nothing_is_happening() {
    // The still world's whole argument, on a frame rather than in a counter: a
    // client that has been standing in a wood for a minute is told about the
    // people and nothing else.
    let mut state = SkapeState::new();
    state.mode = Relevance::OnChange;
    let me = seated(&mut state, 1);
    let middle = state.zone.actors[&me].tile;
    let props = state.zone.props_in_view(middle);

    for _ in 0..20 {
      frame_for(&mut state, me);
    }
    let quiet = frame_for(&mut state, me);
    println!(
      "\n  {props} props in view, {} on a settled frame, {} actors\n",
      quiet.objects.len(),
      quiet.actors.len()
    );
    assert!(quiet.objects.is_empty(), "a still world talked anyway");
    assert!(props > 80, "only {props} props in view");
  }
}
