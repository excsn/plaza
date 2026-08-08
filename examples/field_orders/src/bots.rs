//! The bot commanders. One task drives every bot squad on the field, and each
//! decision is made from [`BattleView`], the same payload every client
//! renders, picking from the server-computed [`BattleView::orders`]. The bot
//! holds no movement rules of its own, so it cannot disagree with the server
//! about what a unit may do; at worst it picks badly.

use std::time::Duration;

use plaza::{
  agent::Agent,
  controller::{query_with, CommandSender, ControllerCommand},
};

use crate::protocol::{is_bot, manhattan, BattleOp, BattlePhase, BattleView, Cell, PlayerId};
use crate::state::BattleState;

pub type BattleCommands = CommandSender<BattleOp, PlayerId, BattleState>;

/// Long enough that a person can watch each order land, well inside the phase
/// deadline so the field is not commanding for it.
pub const THINK: Duration = Duration::from_millis(700);

/// Plays every bot commander on the field: one order per bot per think, so a
/// full bot side advances in parallel at a watchable pace.
pub async fn play_the_bots(tx: BattleCommands, think: Duration) {
  let mut ticker = tokio::time::interval(think);
  loop {
    ticker.tick().await;

    let Ok(view) = query_with(&tx, |state: &BattleState| state.view()).await else {
      return;
    };
    let BattlePhase::Command(army) = view.phase else {
      continue;
    };
    for (bot, _) in view.commanders.iter().filter(|(p, a)| is_bot(*p) && *a == army) {
      let Some(op) = decide(&view, *bot) else {
        continue;
      };
      if tx
        .send(ControllerCommand::SubmitAgentOps {
          agent: Agent::new_bot(*bot),
          ops: vec![op],
        })
        .await
        .is_err()
      {
        return;
      }
    }
  }
}

/// One order for one commander, or `None` when it is not their moment. Strike
/// what can be struck, weakest first; mend the most wounded patient in reach;
/// otherwise close the distance, taking cover on a tie; otherwise hold. Only
/// the commander's own squad is considered, because the guard would refuse
/// anything else. The phase ends itself when the army's set drains, so no bot
/// ever needs `EndPhase`.
pub fn decide(view: &BattleView, me: PlayerId) -> Option<BattleOp> {
  let army = view.commanders.iter().find(|(p, _)| *p == me).map(|(_, a)| *a)?;
  if view.phase != BattlePhase::Command(army) {
    return None;
  }
  let mine = |unit: u8| {
    view
      .units
      .iter()
      .find(|u| u.id == unit)
      .is_some_and(|u| u.owner == me)
  };

  if let Some(orders) = view.orders.iter().filter(|o| mine(o.unit)).find(|o| !o.strike.is_empty()) {
    let target = orders
      .strike
      .iter()
      .copied()
      .min_by_key(|id| view.units.iter().find(|u| u.id == *id).map(|u| u.hp).unwrap_or(i8::MAX))?;
    return Some(BattleOp::Strike { unit: orders.unit, target });
  }

  if let Some(orders) = view.orders.iter().filter(|o| mine(o.unit)).find(|o| !o.heal.is_empty()) {
    let target = orders
      .heal
      .iter()
      .copied()
      .min_by_key(|id| view.units.iter().find(|u| u.id == *id).map(|u| u.hp).unwrap_or(i8::MAX))?;
    return Some(BattleOp::Heal { unit: orders.unit, target });
  }

  let enemies: Vec<Cell> = view.units.iter().filter(|u| u.army != army).map(|u| u.at).collect();
  if let Some(orders) = view
    .orders
    .iter()
    .filter(|o| mine(o.unit))
    .find(|o| !o.march.is_empty())
    && !enemies.is_empty()
  {
    let closing = |cell: &Cell| enemies.iter().map(|e| manhattan(*cell, *e)).min().unwrap_or(i8::MAX);
    let cover = |cell: &Cell| {
      let (x, y) = (cell.0 as usize, cell.1 as usize);
      view.terrain.get(y).and_then(|row| row.get(x)).map(|t| t.defense()).unwrap_or(0)
    };
    let to = orders
      .march
      .iter()
      .copied()
      .min_by_key(|cell| (closing(cell), -(cover(cell) as i16), *cell))?;
    return Some(BattleOp::Move { unit: orders.unit, to });
  }

  view
    .orders
    .iter()
    .filter(|o| mine(o.unit))
    .map(|o| BattleOp::Hold { unit: o.unit })
    .next()
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::logic::BattleLogic;
  use crate::protocol::{Activation, Army, Class, BOT_BASE, MUSTER_TICKS};
  use plaza::state_logic::{LogicInput, StateLogic};

  async fn run(state: &mut BattleState, input: LogicInput<BattleOp, PlayerId>) {
    BattleLogic.process_input(state, input).await.unwrap();
  }

  /// One human against the bot, deployed on the small field.
  async fn solo() -> BattleState {
    let mut state = BattleState::new();
    run(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(1),
    })
    .await;
    run(&mut state, LogicInput::AgentOps {
      source: Agent::new_human(1),
      ops: vec![BattleOp::StartMuster],
    })
    .await;
    for _ in 0..=MUSTER_TICKS {
      run(&mut state, LogicInput::TimeStep {
        delta_time: std::time::Duration::from_millis(20),
      })
      .await;
    }
    state
  }

  #[tokio::test]
  async fn the_bot_waits_outside_its_phase() {
    let state = solo().await;
    assert_eq!(state.army_of(BOT_BASE), Some(Army::Red));
    assert_eq!(decide(&state.view(), BOT_BASE), None, "Red does not act in Blue's phase");
    assert_eq!(decide(&state.view(), 9), None, "a spectator never acts");
  }

  #[tokio::test]
  async fn the_bot_strikes_the_weakest_in_reach() {
    let mut state = solo().await;
    run(&mut state, LogicInput::AgentOps {
      source: Agent::new_human(1),
      ops: vec![BattleOp::EndPhase],
    })
    .await;

    let knight = state.units.iter().find(|u| u.owner == BOT_BASE && u.class == Class::Knight).unwrap().id;
    state.unit_mut(knight).unwrap().at = (4, 0);
    state.unit_mut(2).unwrap().at = (5, 0);
    state.unit_mut(3).unwrap().at = (4, 1);
    state.unit_mut(3).unwrap().hp = 1;

    let op = decide(&state.view(), BOT_BASE);
    assert_eq!(op, Some(BattleOp::Strike { unit: knight, target: 3 }), "the wounded archer first");
  }

  #[tokio::test]
  async fn the_bot_mends_before_it_marches() {
    let mut state = solo().await;
    run(&mut state, LogicInput::AgentOps {
      source: Agent::new_human(1),
      ops: vec![BattleOp::EndPhase],
    })
    .await;

    let healer = state.units.iter().find(|u| u.owner == BOT_BASE && u.class == Class::Healer).unwrap().id;
    let knight = state.units.iter().find(|u| u.owner == BOT_BASE && u.class == Class::Knight).unwrap().id;
    state.unit_mut(healer).unwrap().at = (8, 0);
    state.unit_mut(knight).unwrap().at = (9, 0);
    state.unit_mut(knight).unwrap().hp = 2;

    let op = decide(&state.view(), BOT_BASE);
    assert_eq!(op, Some(BattleOp::Heal { unit: healer, target: knight }));
  }

  #[tokio::test]
  async fn a_commander_decides_for_their_own_squad_alone() {
    let mut state = BattleState::new();
    for player in 1..=4 {
      run(&mut state, LogicInput::AgentJoined {
        agent: Agent::new_human(player),
      })
      .await;
    }
    run(&mut state, LogicInput::AgentOps {
      source: Agent::new_human(1),
      ops: vec![BattleOp::StartMuster],
    })
    .await;
    for _ in 0..=MUSTER_TICKS {
      run(&mut state, LogicInput::TimeStep {
        delta_time: std::time::Duration::from_millis(20),
      })
      .await;
    }

    let Some(BattleOp::Move { unit, .. }) = decide(&state.view(), 3) else {
      panic!("an opening board is a march");
    };
    assert_eq!(state.unit(unit).unwrap().owner, 3, "player 3's decision spends player 3's unit");
  }

  #[tokio::test]
  async fn a_boxed_in_squad_holds_and_the_set_drains() {
    let mut state = solo().await;
    // Strip Blue to one spent-march soldier with nothing in reach.
    state.units.retain(|u| u.id == 2 || u.owner == BOT_BASE);
    state.unit_mut(2).unwrap().activation = Activation::Moved;

    let op = decide(&state.view(), 1);
    assert_eq!(op, Some(BattleOp::Hold { unit: 2 }));
  }
}
