//! The rules. The only place `BattleState` changes.
//!
//! # What had to be hand-written, which is the example's finding
//!
//! `flow_control` manages *sequences*: a turn order that hands the floor to one
//! actor, rounds that count. A command phase is neither. It holds every unit of
//! an army at once, up to sixteen commanders each ordering their own squad in
//! any order, and the phase is over when the **set** of unspent units is empty.
//! That is [`Activation`] plus [`maybe_end_phase`], the same thirty lines at
//! two players and at thirty-two: the set got wider, the shape did not change.

use async_trait::async_trait;
use plaza::agent::Agent;
use plaza::common::fsm::{FsmContext as _, OpsQueue};
use plaza::error::StateLogicError;
use plaza::game_common::flow_control::RoundManager;
use plaza::game_common::scorekeeping::Scorekeeper;
use plaza::session::TargetedOp;
use plaza::state_logic::{LogicInput, LogicOutput, SnapshotRequest, StateLogic};
use tracing::{debug, info, warn};

use crate::map;
use crate::protocol::{
  is_bot, manhattan, on_board_of, Activation, Army, BattleOp, BattlePhase, Cell, Class, MapSize, PlayerId, Refusal,
  RoundSummary, Terrain, Unit, BOT_BASE, INTERMISSION_TICKS, MAX_COMMANDERS,
};
use crate::state::{BattleEvent, BattleState};

type Ctx = OpsQueue<BattleOp, PlayerId>;

#[derive(Debug, Default)]
pub struct BattleLogic;

#[async_trait]
impl StateLogic<BattleOp, PlayerId, BattleState> for BattleLogic {
  async fn process_input(
    &self,
    state: &mut BattleState,
    input: LogicInput<BattleOp, PlayerId>,
  ) -> Result<LogicOutput<BattleOp, PlayerId>, StateLogicError> {
    let mut ctx = Ctx::new();
    let mut resnapshot = false;

    match input {
      LogicInput::AgentJoined { agent } => {
        resnapshot = muster(state, &agent, &mut ctx);
      }

      LogicInput::AgentLeft { agent_id } => {
        resnapshot = depart(state, agent_id, &mut ctx);
      }

      LogicInput::AgentOps { source, ops } => {
        let Some(player) = source.id_cloned() else {
          return Err(StateLogicError::InvalidOperation("ops from an unidentified agent".into()));
        };
        for op in ops {
          resnapshot |= match op {
            BattleOp::Move { unit, to } => march(state, player, unit, to, &mut ctx),
            BattleOp::Strike { unit, target } => strike(state, player, unit, target, &mut ctx),
            BattleOp::Heal { unit, target } => heal(state, player, unit, target, &mut ctx),
            BattleOp::Hold { unit } => hold(state, player, unit, &mut ctx),
            BattleOp::EndPhase => end_phase_ordered(state, player, &mut ctx),
            BattleOp::SetMapSize(choice) => set_map_size(state, player, choice, &mut ctx),
            BattleOp::StartMuster => start_muster(state, player, &mut ctx),
            _ => false,
          };
        }
      }

      LogicInput::TimeStep { .. } => {
        state.tick += 1;
        resnapshot = run_due_events(state, &mut ctx);
      }
    }

    let output = LogicOutput::ops(ctx.into_ops());
    if resnapshot {
      // Open information, so one view serves the room and the controller builds
      // it once. `night_watch` is the opposite case and pays per recipient to
      // keep its roles dark.
      let everyone: Vec<Agent<PlayerId>> = state.agents.values().cloned().collect();
      return Ok(output.and_snapshot(SnapshotRequest::uniform(everyone)));
    }
    Ok(output)
  }
}

/// May `player` command `unit` right now? The battle's whole authorization, in
/// one place. Ownership is per squad: a teammate's unit answers to its own
/// commander alone.
fn guard(state: &BattleState, player: PlayerId, unit: u8) -> Result<(), Refusal> {
  let Some(army) = state.army_of(player) else {
    return Err(Refusal::Spectating);
  };
  if *state.phase.current() != BattlePhase::Command(army) {
    return Err(Refusal::NotYourPhase);
  }
  let Some(subject) = state.unit(unit) else {
    return Err(Refusal::NoSuchTarget);
  };
  if subject.owner != player {
    return Err(Refusal::NotYourUnit);
  }
  Ok(())
}

fn refuse(ctx: &mut Ctx, player: PlayerId, why: Refusal) -> bool {
  warn!(player, ?why, "order refused");
  ctx
    .ops_q()
    .push(TargetedOp::new_system_to(player, vec![BattleOp::Refused(why)]));
  false
}

/// An arriving commander joins the lobby. Nothing counts down by itself: the
/// host (the first mustered) picks the field and starts it, like any lobby.
/// A mid-battle joiner watches, mustered for the next deploy.
fn muster(state: &mut BattleState, agent: &Agent<PlayerId>, ctx: &mut Ctx) -> bool {
  let Some(player) = agent.id_cloned() else {
    return false;
  };
  if state.agents.contains_key(&player) {
    return false;
  }
  state.agents.insert(player, agent.clone());

  if state.mustered.len() >= MAX_COMMANDERS {
    info!(player, "the muster is full; connected as a spectator");
    return true;
  }
  state.mustered.push(player);
  ctx
    .ops_q()
    .push(TargetedOp::new_system_to(player, vec![BattleOp::YouAre(player)]));
  info!(player, mustered = state.mustered.len(), host = ?state.host(), "commander mustered");
  true
}

/// The host's field pick, only while the lobby is open: the countdown locks
/// the settings it started under.
fn set_map_size(state: &mut BattleState, player: PlayerId, choice: Option<MapSize>, ctx: &mut Ctx) -> bool {
  if *state.phase.current() != BattlePhase::Mustering {
    return refuse(ctx, player, Refusal::NotYourPhase);
  }
  if state.host() != Some(player) || state.muster_due.is_some() {
    return refuse(ctx, player, Refusal::NotHost);
  }
  state.map_choice = choice;
  info!(player, ?choice, "the host set the field");
  true
}

fn start_muster(state: &mut BattleState, player: PlayerId, ctx: &mut Ctx) -> bool {
  if *state.phase.current() != BattlePhase::Mustering {
    return refuse(ctx, player, Refusal::NotYourPhase);
  }
  if state.host() != Some(player) || state.muster_due.is_some() {
    return refuse(ctx, player, Refusal::NotHost);
  }
  state.muster_due = Some(state.tick + state.muster_ticks);
  state
    .timeouts
    .schedule_after(state.tick, state.muster_ticks, &state.phase, BattleEvent::MusterCloses);
  info!(player, in_ticks = state.muster_ticks, "the host started the countdown");
  true
}

/// A departing commander's squad marches home; a departing side concedes.
fn depart(state: &mut BattleState, player: PlayerId, ctx: &mut Ctx) -> bool {
  state.agents.remove(&player);
  state.mustered.retain(|p| *p != player);
  let Some(army) = state.armies.remove(&player) else {
    return true;
  };
  state.wins.forget_player(&player);
  state.units.retain(|u| u.owner != player);
  info!(player, "commander left; their squad marches home");

  if matches!(*state.phase.current(), BattlePhase::Command(_)) && state.army_size(army) == 0 {
    end_round(state, ctx);
    battle_over(state, army.other(), "the last squads marched home", ctx);
  } else {
    maybe_end_phase(state, ctx);
  }
  true
}

/// Deploys the field for whoever mustered. The map fits the muster, the sides
/// alternate in muster order (swapping which opens every game), and the bot
/// takes one squad to even an odd count, which is also how a lone commander
/// gets an opponent.
fn deploy_field(state: &mut BattleState, ctx: &mut Ctx) {
  state.games += 1;
  state.muster_due = None;
  // The larger of the host's pick and what the muster needs: a pick may make
  // the field roomier, never too small for the squads.
  let fits = MapSize::for_commanders(state.mustered.len());
  state.map = state.map_choice.map_or(fits, |picked| picked.max(fits));

  let first_is_blue = state.games % 2 == 1;
  let humans = state.mustered.clone();
  let mut roster: Vec<(PlayerId, Army)> = humans
    .iter()
    .enumerate()
    .map(|(i, p)| (*p, if (i % 2 == 0) == first_is_blue { Army::Blue } else { Army::Red }))
    .collect();
  if roster.len() % 2 == 1 {
    let blue = roster.iter().filter(|(_, a)| *a == Army::Blue).count();
    let short = if blue * 2 > roster.len() { Army::Red } else { Army::Blue };
    roster.push((BOT_BASE, short));
    info!(?short, "the bot evens the sides");
  }

  state.armies = roster.iter().copied().collect();
  for (player, _) in &roster {
    if !is_bot(*player) && state.wins.get_score(player).is_none() {
      state.wins.set_score(player, 0);
    }
  }
  state.units = map::deploy(state.map, &roster);
  state.fallen.clear();
  state.felled_this_round = 0;
  state.victor = None;
  state.rounds.reset();

  info!(game = state.games, map = ?state.map, squads = roster.len(), "the field deploys");
  begin_round(state, ctx);
}

fn begin_round(state: &mut BattleState, ctx: &mut Ctx) {
  if let Err(reason) = state.rounds.start_next_round(ctx) {
    debug!(%reason, "no round to start");
    return;
  }
  state.felled_this_round = 0;
  begin_command(state, Army::Blue, ctx);
}

fn begin_command(state: &mut BattleState, army: Army, ctx: &mut Ctx) {
  for unit in state.units.iter_mut().filter(|u| u.army == army) {
    unit.activation = Activation::Fresh;
  }
  let ticks = state.map.side_ticks(state.side_ticks);
  state.phase.transition_with(
    BattlePhase::Command(army),
    ctx,
    BattleOp::PhaseChanged,
    Some(format!("{army:?} commands")),
    Some(state.tick_interval * ticks as u32),
  );
  state
    .timeouts
    .schedule_after(state.tick, ticks, &state.phase, BattleEvent::PhaseExpires);
}

fn march(state: &mut BattleState, player: PlayerId, unit: u8, to: Cell, ctx: &mut Ctx) -> bool {
  if let Err(why) = guard(state, player, unit) {
    return refuse(ctx, player, why);
  }
  let subject = *state.unit(unit).expect("guarded");
  if subject.activation != Activation::Fresh {
    return refuse(ctx, player, Refusal::Spent);
  }
  if !map::reachable(state.map, &state.units, &subject).contains(&to) {
    let (w, h) = state.map.dims();
    let blocked =
      !on_board_of(to, w, h) || map::terrain_at(state.map, to) == Terrain::Rock || state.occupied(to);
    return refuse(ctx, player, if blocked { Refusal::Occupied } else { Refusal::OutOfReach });
  }

  let subject = state.unit_mut(unit).expect("guarded");
  subject.at = to;
  subject.activation = Activation::Moved;
  ctx
    .ops_q()
    .push(TargetedOp::new_system_all(vec![BattleOp::Marched { unit, to }]));
  info!(unit, ?to, "marched");
  true
}

fn strike(state: &mut BattleState, player: PlayerId, unit: u8, target: u8, ctx: &mut Ctx) -> bool {
  if let Err(why) = guard(state, player, unit) {
    return refuse(ctx, player, why);
  }
  let subject = *state.unit(unit).expect("guarded");
  if subject.activation == Activation::Done {
    return refuse(ctx, player, Refusal::Spent);
  }
  if !subject.class.armed() {
    return refuse(ctx, player, Refusal::NoSuchTarget);
  }
  let Some(victim) = state.unit(target).copied() else {
    return refuse(ctx, player, Refusal::NoSuchTarget);
  };
  if victim.army == subject.army {
    return refuse(ctx, player, Refusal::NoSuchTarget);
  }
  let dist = manhattan(subject.at, victim.at);
  if dist != subject.class.stats().range {
    return refuse(ctx, player, Refusal::OutOfReach);
  }

  state.unit_mut(unit).expect("guarded").activation = Activation::Done;
  let felled = land_blow(state, &subject, &victim, false, ctx);
  // The survivor answers, but only from its own reach and only armed: a
  // knight cannot answer the archer two cells out, the archer cannot answer
  // the knight in its face, and a healer answers nothing at all.
  if !felled && victim.class.armed() && dist == victim.class.stats().range {
    land_blow(state, &victim, &subject, true, ctx);
  }

  let striking = subject.army;
  if state.army_size(striking.other()) == 0 {
    end_round(state, ctx);
    battle_over(state, striking, "the enemy army is routed", ctx);
    return true;
  }
  if state.army_size(striking) == 0 {
    end_round(state, ctx);
    battle_over(state, striking.other(), "the counterstrike felled the last attacker", ctx);
    return true;
  }
  maybe_end_phase(state, ctx);
  true
}

/// One blow from `from` upon `upon`, broadcast as it lands. Terrain under the
/// defender blunts it. Returns whether the defender fell.
fn land_blow(state: &mut BattleState, from: &Unit, upon: &Unit, counter: bool, ctx: &mut Ctx) -> bool {
  let damage = (from.class.stats().atk - map::terrain_at(state.map, upon.at).defense()).max(0);
  let struck = state.unit_mut(upon.id).expect("checked");
  struck.hp -= damage;
  let hp_left = struck.hp.max(0);
  let felled = struck.hp <= 0;
  if felled {
    state.units.retain(|u| u.id != upon.id);
    state.fallen.push((upon.id, upon.army));
    state.felled_this_round += 1;
  }
  ctx.ops_q().push(TargetedOp::new_system_all(vec![BattleOp::Struck {
    unit: from.id,
    target: upon.id,
    hp_left,
    felled,
    counter,
  }]));
  info!(unit = from.id, target = upon.id, hp_left, felled, counter, "struck");
  felled
}

/// A mend: the healer's whole action. Ends the activation like a strike, and
/// nothing answers a bandage.
fn heal(state: &mut BattleState, player: PlayerId, unit: u8, target: u8, ctx: &mut Ctx) -> bool {
  if let Err(why) = guard(state, player, unit) {
    return refuse(ctx, player, why);
  }
  let subject = *state.unit(unit).expect("guarded");
  if subject.activation == Activation::Done {
    return refuse(ctx, player, Refusal::Spent);
  }
  if subject.class != Class::Healer {
    return refuse(ctx, player, Refusal::NoSuchTarget);
  }
  let Some(patient) = state.unit(target).copied() else {
    return refuse(ctx, player, Refusal::NoSuchTarget);
  };
  if patient.army != subject.army || patient.id == subject.id || patient.hp >= patient.class.stats().hp {
    return refuse(ctx, player, Refusal::NoSuchTarget);
  }
  if manhattan(subject.at, patient.at) != subject.class.stats().range {
    return refuse(ctx, player, Refusal::OutOfReach);
  }

  state.unit_mut(unit).expect("guarded").activation = Activation::Done;
  let mend = subject.class.stats().atk;
  let patient = state.unit_mut(target).expect("checked");
  patient.hp = (patient.hp + mend).min(patient.class.stats().hp);
  let hp_now = patient.hp;
  ctx
    .ops_q()
    .push(TargetedOp::new_system_all(vec![BattleOp::Healed { unit, target, hp_now }]));
  info!(unit, target, hp_now, "mended");
  maybe_end_phase(state, ctx);
  true
}

fn hold(state: &mut BattleState, player: PlayerId, unit: u8, ctx: &mut Ctx) -> bool {
  if let Err(why) = guard(state, player, unit) {
    return refuse(ctx, player, why);
  }
  let subject = state.unit_mut(unit).expect("guarded");
  if subject.activation == Activation::Done {
    return refuse(ctx, player, Refusal::Spent);
  }
  subject.activation = Activation::Done;
  debug!(unit, "holds");
  maybe_end_phase(state, ctx);
  true
}

/// A commander ending their army's phase early; every unacted unit forfeits,
/// their teammates' included. One order, army-wide, because a phase belongs to
/// a side and not to a squad.
fn end_phase_ordered(state: &mut BattleState, player: PlayerId, ctx: &mut Ctx) -> bool {
  let Some(army) = state.army_of(player) else {
    return refuse(ctx, player, Refusal::Spectating);
  };
  if *state.phase.current() != BattlePhase::Command(army) {
    return refuse(ctx, player, Refusal::NotYourPhase);
  }
  end_command(state, army, ctx);
  true
}

/// The set check that is the whole within-side structure: the phase is over
/// when no unit of the commanding army has anything left to do. At two squads
/// that set is eight wide; at thirty-two commanders it is sixty-four, and the
/// check has not changed.
fn maybe_end_phase(state: &mut BattleState, ctx: &mut Ctx) {
  let BattlePhase::Command(army) = *state.phase.current() else {
    return;
  };
  let anyone_left = state
    .units
    .iter()
    .any(|u| u.army == army && u.activation != Activation::Done);
  if !anyone_left {
    end_command(state, army, ctx);
  }
}

fn end_command(state: &mut BattleState, army: Army, ctx: &mut Ctx) {
  match army {
    Army::Blue => begin_command(state, Army::Red, ctx),
    Army::Red => {
      end_round(state, ctx);
      begin_round(state, ctx);
    }
  }
}

fn end_round(state: &mut BattleState, ctx: &mut Ctx) {
  if state.rounds.round_in_progress() {
    let summary = RoundSummary {
      felled: state.felled_this_round,
    };
    state.rounds.end_round_with(ctx, "both armies have commanded", Some(summary));
  }
}

fn battle_over(state: &mut BattleState, winner: Army, reason: &str, ctx: &mut Ctx) {
  state.victor = Some(winner);
  // The hint is the intermission, so a client can count down to the return
  // to muster instead of sitting on an unexplained result screen.
  state.phase.transition_with(
    BattlePhase::Over,
    ctx,
    BattleOp::PhaseChanged,
    Some(reason.into()),
    Some(state.tick_interval * INTERMISSION_TICKS as u32),
  );
  for commander in state.commanders_of(winner) {
    if !is_bot(commander) {
      state.wins.increment_score(&commander, 1);
    }
  }
  ctx
    .ops_q()
    .push(TargetedOp::new_system_all(vec![BattleOp::BattleOver { winner }]));
  info!(?winner, %reason, "battle over");

  state
    .timeouts
    .schedule_after(state.tick, INTERMISSION_TICKS, &state.phase, BattleEvent::Redeploy);
}

/// Fires whatever came due; the scheduler already dropped what the battle
/// overtook.
fn run_due_events(state: &mut BattleState, ctx: &mut Ctx) -> bool {
  let mut changed = false;

  for due in state.timeouts.due(state.tick, &state.phase) {
    match due {
      BattleEvent::MusterCloses => {
        if state.mustered.is_empty() {
          state.muster_due = None;
          debug!("muster closed on nobody; disarmed");
          continue;
        }
        deploy_field(state, ctx);
        changed = true;
      }

      BattleEvent::PhaseExpires => {
        let BattlePhase::Command(army) = *state.phase.current() else {
          continue;
        };
        info!(?army, "the command phase ran out; the rest do not act");
        end_command(state, army, ctx);
        changed = true;
      }

      BattleEvent::Redeploy => {
        info!("the result has been read; back to the lobby");
        state
          .phase
          .transition_with(BattlePhase::Mustering, ctx, BattleOp::PhaseChanged, None, None);
        state.armies.clear();
        state.muster_due = None;
        changed = true;
      }
    }
  }

  changed
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::protocol::{MUSTER_TICKS, SIDE_TICKS, SQUAD};

  async fn run(state: &mut BattleState, input: LogicInput<BattleOp, PlayerId>) {
    BattleLogic.process_input(state, input).await.unwrap();
  }

  async fn tick(state: &mut BattleState) {
    run(state, LogicInput::TimeStep {
      delta_time: std::time::Duration::from_millis(20),
    })
    .await;
  }

  async fn ticks(state: &mut BattleState, n: u64) {
    for _ in 0..n {
      tick(state).await;
    }
  }

  async fn act(state: &mut BattleState, who: PlayerId, op: BattleOp) {
    run(state, LogicInput::AgentOps {
      source: Agent::new_human(who),
      ops: vec![op],
    })
    .await;
  }

  async fn join(state: &mut BattleState, player: PlayerId) {
    run(state, LogicInput::AgentJoined {
      agent: Agent::new_human(player),
    })
    .await;
  }

  async fn leave(state: &mut BattleState, player: PlayerId) {
    run(state, LogicInput::AgentLeft { agent_id: player }).await;
  }

  /// Musters `n` commanders, has the host start the lobby, and runs the
  /// countdown out. Game 1 alternates Blue/Red in join order, so player 1 is
  /// Blue.
  async fn field_of(n: u32) -> BattleState {
    let mut state = BattleState::new();
    for player in 1..=n {
      join(&mut state, player).await;
    }
    act(&mut state, 1, BattleOp::StartMuster).await;
    ticks(&mut state, MUSTER_TICKS + 1).await;
    state
  }

  /// The 1v1 small field. Blue: knight 1 at (1,2), soldier 2 at (0,1),
  /// archer 3 at (0,3), healer 4 at (0,2); Red mirrored, ids 5 through 8.
  async fn camp() -> BattleState {
    field_of(2).await
  }

  fn place(state: &mut BattleState, unit: u8, at: Cell) {
    state.unit_mut(unit).unwrap().at = at;
  }

  #[tokio::test]
  async fn the_muster_counts_down_and_the_field_deploys() {
    let mut state = BattleState::new();
    join(&mut state, 1).await;
    join(&mut state, 2).await;
    assert_eq!(*state.phase.current(), BattlePhase::Mustering);
    assert!(state.muster_due.is_none(), "nothing counts down until the host says so");
    ticks(&mut state, MUSTER_TICKS + 5).await;
    assert_eq!(*state.phase.current(), BattlePhase::Mustering, "the lobby waits");

    act(&mut state, 1, BattleOp::StartMuster).await;
    assert!(state.muster_due.is_some(), "the host armed the countdown");
    ticks(&mut state, MUSTER_TICKS + 1).await;
    assert_eq!(*state.phase.current(), BattlePhase::Command(Army::Blue));
    assert_eq!(state.map, MapSize::Small);
    assert_eq!(state.units.len(), 2 * SQUAD);
    assert_eq!(state.army_of(1), Some(Army::Blue));
    assert_eq!(state.army_of(2), Some(Army::Red));
    assert_eq!(state.unit(1).unwrap().class, Class::Knight);
    assert_eq!(state.unit(8).unwrap().class, Class::Healer);
    assert_eq!(state.rounds.current_round(), 1);
  }

  #[tokio::test]
  async fn a_lone_commander_gets_the_bot() {
    let state = field_of(1).await;
    assert_eq!(*state.phase.current(), BattlePhase::Command(Army::Blue));
    assert_eq!(state.army_of(BOT_BASE), Some(Army::Red), "the bot evens the sides");
    assert_eq!(state.units.len(), 2 * SQUAD);
    let bot_units = state.units.iter().filter(|u| u.owner == BOT_BASE).count();
    assert_eq!(bot_units, SQUAD);
  }

  #[tokio::test]
  async fn the_field_scales_with_the_muster() {
    let state = field_of(4).await;
    assert_eq!(state.map, MapSize::Medium);
    assert_eq!(state.units.len(), 4 * SQUAD);

    let state = field_of(9).await;
    assert_eq!(state.map, MapSize::Xlarge);
    assert_eq!(state.armies.len(), 10, "nine humans and the evening bot");
    assert_eq!(state.units.len(), 10 * SQUAD);
  }

  #[tokio::test]
  async fn thirty_two_commanders_take_the_xlarge_field() {
    let state = field_of(32).await;
    assert_eq!(state.map, MapSize::Xlarge);
    assert_eq!(state.units.len(), 32 * SQUAD, "one hundred and twenty-eight units");
    let blue = state.units.iter().filter(|u| u.army == Army::Blue).count();
    assert_eq!(blue, 16 * SQUAD);
    assert!(state.armies.keys().all(|p| !is_bot(*p)), "an even muster needs no bot");
  }

  #[tokio::test]
  async fn orders_are_refused_outside_your_phase_or_squad() {
    let mut state = field_of(4).await;
    // Game 1, join order alternates: 1 and 3 are Blue, 2 and 4 Red.
    assert_eq!(state.army_of(3), Some(Army::Blue));
    let mine = state.units.iter().find(|u| u.owner == 1).unwrap().id;
    let teammates = state.units.iter().find(|u| u.owner == 3).unwrap().id;
    let enemys = state.units.iter().find(|u| u.owner == 2).unwrap().id;

    let hold = |unit| BattleOp::Hold { unit };
    act(&mut state, 2, hold(enemys)).await;
    assert_eq!(state.unit(enemys).unwrap().activation, Activation::Fresh, "not Red's phase");

    act(&mut state, 1, hold(teammates)).await;
    assert_eq!(
      state.unit(teammates).unwrap().activation,
      Activation::Fresh,
      "a teammate's unit is not yours to spend"
    );

    act(&mut state, 1, hold(mine)).await;
    assert_eq!(state.unit(mine).unwrap().activation, Activation::Done);
  }

  #[tokio::test]
  async fn a_unit_marches_once_strikes_once_and_is_spent() {
    let mut state = camp().await;
    place(&mut state, 2, (7, 0));
    place(&mut state, 6, (9, 0));

    act(&mut state, 1, BattleOp::Move { unit: 2, to: (8, 0) }).await;
    assert_eq!(state.unit(2).unwrap().at, (8, 0));
    assert_eq!(state.unit(2).unwrap().activation, Activation::Moved);

    act(&mut state, 1, BattleOp::Move { unit: 2, to: (7, 0) }).await;
    assert_eq!(state.unit(2).unwrap().at, (8, 0), "one march per activation");

    act(&mut state, 1, BattleOp::Strike { unit: 2, target: 6 }).await;
    assert_eq!(state.unit(6).unwrap().hp, 4);
    assert_eq!(state.unit(2).unwrap().activation, Activation::Done);

    act(&mut state, 1, BattleOp::Strike { unit: 2, target: 6 }).await;
    assert_eq!(state.unit(6).unwrap().hp, 4, "one strike per activation");
  }

  #[tokio::test]
  async fn marching_respects_reach_terrain_and_occupancy() {
    let mut state = camp().await;

    act(&mut state, 1, BattleOp::Move { unit: 1, to: (6, 2) }).await;
    assert_eq!(state.unit(1).unwrap().at, (1, 2), "past the knight's movement");

    act(&mut state, 1, BattleOp::Move { unit: 1, to: (4, 2) }).await;
    assert_eq!(state.unit(1).unwrap().at, (1, 2), "rock is not ground");

    act(&mut state, 1, BattleOp::Move { unit: 1, to: (-1, 2) }).await;
    assert_eq!(state.unit(1).unwrap().at, (1, 2), "the board has an edge");

    act(&mut state, 1, BattleOp::Move { unit: 1, to: (0, 1) }).await;
    assert_eq!(state.unit(1).unwrap().at, (1, 2), "soldier 2 is standing there");

    act(&mut state, 1, BattleOp::Move { unit: 1, to: (3, 2) }).await;
    assert_eq!(state.unit(1).unwrap().at, (3, 2));
  }

  #[tokio::test]
  async fn the_counterstrike_answers_only_at_matched_reach_and_armed() {
    let mut state = camp().await;

    // The archer pelts from two out; a soldier answers at one, so no answer.
    place(&mut state, 3, (7, 0));
    place(&mut state, 6, (9, 0));
    act(&mut state, 1, BattleOp::Strike { unit: 3, target: 6 }).await;
    assert_eq!(state.unit(6).unwrap().hp, 4);
    assert_eq!(state.unit(3).unwrap().hp, 5, "nothing reaches back two cells");

    // A soldier on the enemy healer: a healer answers nothing.
    place(&mut state, 6, (5, 0));
    place(&mut state, 8, (9, 0));
    place(&mut state, 2, (9, 1));
    act(&mut state, 1, BattleOp::Strike { unit: 2, target: 8 }).await;
    assert_eq!(state.unit(8).unwrap().hp, 3);
    assert_eq!(state.unit(2).unwrap().hp, 6, "a bandage is not a weapon");
  }

  #[tokio::test]
  async fn matched_reach_means_an_answer() {
    let mut state = camp().await;
    place(&mut state, 2, (7, 0));
    place(&mut state, 6, (8, 0));

    act(&mut state, 1, BattleOp::Strike { unit: 2, target: 6 }).await;
    assert_eq!(state.unit(6).unwrap().hp, 4, "the blow landed");
    assert_eq!(state.unit(2).unwrap().hp, 4, "and was answered");
  }

  #[tokio::test]
  async fn the_forest_blunts_the_blow() {
    let mut state = camp().await;
    place(&mut state, 5, (7, 3));
    place(&mut state, 2, (7, 2));

    act(&mut state, 1, BattleOp::Strike { unit: 2, target: 5 }).await;
    assert_eq!(state.unit(5).unwrap().hp, 7, "the forest ate one of two");
    assert_eq!(state.unit(2).unwrap().hp, 3, "the knight's answer, unblunted on the plain");
  }

  #[tokio::test]
  async fn a_felled_unit_does_not_answer() {
    let mut state = camp().await;
    place(&mut state, 2, (7, 0));
    place(&mut state, 6, (8, 0));
    state.unit_mut(6).unwrap().hp = 2;

    act(&mut state, 1, BattleOp::Strike { unit: 2, target: 6 }).await;
    assert!(state.unit(6).is_none());
    assert!(state.fallen.contains(&(6, Army::Red)));
    assert_eq!(state.unit(2).unwrap().hp, 6, "the dead do not strike back");
  }

  #[tokio::test]
  async fn a_healer_mends_capped_and_nothing_answers_a_bandage() {
    let mut state = camp().await;
    place(&mut state, 1, (3, 2));
    place(&mut state, 4, (3, 3));
    state.unit_mut(1).unwrap().hp = 7;

    act(&mut state, 1, BattleOp::Heal { unit: 4, target: 1 }).await;
    assert_eq!(state.unit(1).unwrap().hp, 8, "mended two, capped at the class ceiling");
    assert_eq!(state.unit(4).unwrap().activation, Activation::Done);
    assert_eq!(state.unit(4).unwrap().hp, 5, "nothing answered");
  }

  #[tokio::test]
  async fn a_healer_cannot_strike_and_a_mend_needs_a_wounded_ally() {
    let mut state = camp().await;
    place(&mut state, 4, (8, 2));

    act(&mut state, 1, BattleOp::Strike { unit: 4, target: 8 }).await;
    assert_eq!(state.unit(8).unwrap().hp, 5, "a healer carries no weapon");
    assert_eq!(state.unit(4).unwrap().activation, Activation::Fresh, "the refusal spent nothing");

    act(&mut state, 1, BattleOp::Heal { unit: 4, target: 8 }).await;
    assert_eq!(state.unit(8).unwrap().hp, 5, "an enemy is not a patient");

    place(&mut state, 1, (7, 2));
    act(&mut state, 1, BattleOp::Heal { unit: 4, target: 1 }).await;
    assert_eq!(state.unit(1).unwrap().hp, 8, "the unhurt are not patients either");
    assert_eq!(state.unit(4).unwrap().activation, Activation::Fresh);
  }

  #[tokio::test]
  async fn the_phase_ends_itself_when_the_last_unit_is_done() {
    let mut state = camp().await;
    for unit in [1, 2, 3] {
      act(&mut state, 1, BattleOp::Hold { unit }).await;
      assert_eq!(*state.phase.current(), BattlePhase::Command(Army::Blue), "units remain");
    }
    act(&mut state, 1, BattleOp::Hold { unit: 4 }).await;
    assert_eq!(
      *state.phase.current(),
      BattlePhase::Command(Army::Red),
      "the set emptied, so the phase ended without an order to end it"
    );
  }

  #[tokio::test]
  async fn end_phase_forfeits_the_unacted_across_every_squad() {
    let mut state = field_of(4).await;
    act(&mut state, 1, BattleOp::EndPhase).await;
    assert_eq!(
      *state.phase.current(),
      BattlePhase::Command(Army::Red),
      "one commander's order ends the army's phase, teammate's squad included"
    );
  }

  #[tokio::test]
  async fn an_idle_army_is_ended_by_the_deadline() {
    let mut state = camp().await;
    ticks(&mut state, SIDE_TICKS + 1).await;
    assert_eq!(*state.phase.current(), BattlePhase::Command(Army::Red));
  }

  #[tokio::test]
  async fn the_stale_deadline_does_not_end_the_phase_that_replaced_its_own() {
    // Blue ends early, so Blue's deadline and Red's come due on the same tick.
    // Exactly one phase must end there: the stale one is dropped inside the
    // scheduler, and a double end would put Red back on the field in round 2.
    let mut state = camp().await;
    act(&mut state, 1, BattleOp::EndPhase).await;
    assert_eq!(*state.phase.current(), BattlePhase::Command(Army::Red));

    ticks(&mut state, SIDE_TICKS + 1).await;
    assert_eq!(*state.phase.current(), BattlePhase::Command(Army::Blue));
    assert_eq!(state.rounds.current_round(), 2, "one end, one round turned");
  }

  #[tokio::test]
  async fn rounds_are_unbounded() {
    let mut state = camp().await;
    for _ in 0..3 {
      act(&mut state, 1, BattleOp::EndPhase).await;
      act(&mut state, 2, BattleOp::EndPhase).await;
    }
    assert_eq!(state.rounds.current_round(), 4);
    assert!(!state.rounds.is_finished(), "the manager never says stop; a rout does");
  }

  #[tokio::test]
  async fn routing_the_last_unit_takes_the_field() {
    let mut state = camp().await;
    state.units.retain(|u| u.army == Army::Blue || u.id == 5);
    state.unit_mut(5).unwrap().hp = 3;
    place(&mut state, 5, (8, 2));
    place(&mut state, 1, (8, 1));

    act(&mut state, 1, BattleOp::Strike { unit: 1, target: 5 }).await;

    assert_eq!(*state.phase.current(), BattlePhase::Over);
    assert_eq!(state.view().winner, Some(Army::Blue));
    assert_eq!(state.wins.get_score(&1), Some(1));
  }

  #[tokio::test]
  async fn a_leaving_squad_marches_home_and_a_leaving_side_concedes() {
    let mut state = field_of(4).await;
    // Blue is players 1 and 3.
    leave(&mut state, 1).await;
    assert!(state.units.iter().all(|u| u.owner != 1), "the squad marched home");
    assert!(
      matches!(*state.phase.current(), BattlePhase::Command(_)),
      "the battle carries on: Blue still has a squad"
    );

    leave(&mut state, 3).await;
    assert_eq!(*state.phase.current(), BattlePhase::Over);
    assert_eq!(state.view().winner, Some(Army::Red));
    assert_eq!(state.wins.get_score(&2), Some(1), "every red commander scores");
    assert_eq!(state.wins.get_score(&4), Some(1));
    assert!(state.wins.get_score(&1).is_none(), "the leavers are off the board");
  }

  #[tokio::test]
  async fn the_intermission_returns_to_muster_and_the_sides_swap() {
    let mut state = camp().await;
    leave(&mut state, 2).await;
    assert_eq!(*state.phase.current(), BattlePhase::Over);

    ticks(&mut state, INTERMISSION_TICKS + 1).await;
    assert_eq!(*state.phase.current(), BattlePhase::Mustering);
    assert!(state.muster_due.is_none(), "back to the lobby, unarmed: the host starts the rematch");

    act(&mut state, 1, BattleOp::StartMuster).await;
    ticks(&mut state, MUSTER_TICKS + 1).await;
    assert_eq!(state.games, 2);
    assert_eq!(state.army_of(1), Some(Army::Red), "game two opens the other way");
    assert_eq!(state.army_of(BOT_BASE), Some(Army::Blue));
  }

  #[tokio::test]
  async fn a_mid_battle_joiner_watches_and_fights_the_next_deploy() {
    let mut state = camp().await;
    join(&mut state, 9).await;
    assert!(state.mustered.contains(&9), "mustered for the next field");
    assert_eq!(state.army_of(9), None);

    act(&mut state, 9, BattleOp::EndPhase).await;
    assert_eq!(*state.phase.current(), BattlePhase::Command(Army::Blue), "a spectator ends nothing");

    leave(&mut state, 2).await;
    ticks(&mut state, INTERMISSION_TICKS + 1).await;
    act(&mut state, 1, BattleOp::StartMuster).await;
    ticks(&mut state, MUSTER_TICKS + 1).await;
    assert_eq!(state.games, 2);
    assert!(state.army_of(9).is_some(), "and the next field seats them");
    assert_eq!(state.armies.len(), 2, "two humans, no bot needed");
  }

  #[tokio::test]
  async fn the_host_picks_the_field_and_the_pick_never_shrinks() {
    let mut state = BattleState::new();
    for player in [1, 2] {
      join(&mut state, player).await;
    }
    act(&mut state, 1, BattleOp::SetMapSize(Some(MapSize::Xlarge))).await;
    act(&mut state, 1, BattleOp::StartMuster).await;
    ticks(&mut state, MUSTER_TICKS + 1).await;
    assert_eq!(state.map, MapSize::Xlarge, "a pair may duel on the biggest field");
    assert_eq!(state.units.len(), 2 * SQUAD);

    let mut state = BattleState::new();
    for player in 1..=5 {
      join(&mut state, player).await;
    }
    act(&mut state, 1, BattleOp::SetMapSize(Some(MapSize::Small))).await;
    act(&mut state, 1, BattleOp::StartMuster).await;
    ticks(&mut state, MUSTER_TICKS + 1).await;
    assert_eq!(state.map, MapSize::Large, "five squads cannot squeeze onto Small");
  }

  #[tokio::test]
  async fn the_lobby_is_the_hosts_and_locks_when_the_countdown_runs() {
    let mut state = BattleState::new();
    join(&mut state, 1).await;
    join(&mut state, 2).await;

    act(&mut state, 2, BattleOp::SetMapSize(Some(MapSize::Large))).await;
    assert_eq!(state.map_choice, None, "only the host sets the field");
    act(&mut state, 2, BattleOp::StartMuster).await;
    assert!(state.muster_due.is_none(), "only the host starts it");

    act(&mut state, 1, BattleOp::StartMuster).await;
    act(&mut state, 1, BattleOp::SetMapSize(Some(MapSize::Large))).await;
    assert_eq!(state.map_choice, None, "the countdown locked the settings");
  }

  #[tokio::test]
  async fn a_leaving_host_hands_the_lobby_on() {
    let mut state = BattleState::new();
    join(&mut state, 1).await;
    join(&mut state, 2).await;
    leave(&mut state, 1).await;

    assert_eq!(state.host(), Some(2));
    act(&mut state, 2, BattleOp::SetMapSize(Some(MapSize::Medium))).await;
    act(&mut state, 2, BattleOp::StartMuster).await;
    ticks(&mut state, MUSTER_TICKS + 1).await;
    assert_eq!(state.map, MapSize::Medium, "the new host's pick held");
  }

  #[tokio::test]
  async fn the_view_carries_the_commanding_armys_options() {
    let mut state = camp().await;
    let view = state.view();
    assert_eq!(view.orders.len(), SQUAD, "every Blue unit has options");
    assert!(view.orders.iter().all(|o| !o.march.is_empty()));

    place(&mut state, 1, (3, 2));
    state.unit_mut(1).unwrap().hp = 4;
    place(&mut state, 4, (3, 3));
    let view = state.view();
    let healer = view.orders.iter().find(|o| o.unit == 4).unwrap();
    assert_eq!(healer.heal, vec![1], "the wounded knight is on the mend list");

    act(&mut state, 1, BattleOp::Hold { unit: 1 }).await;
    let view = state.view();
    assert!(view.orders.iter().all(|o| o.unit != 1), "a spent unit offers nothing");
  }
}
