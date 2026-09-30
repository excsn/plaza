//! Villagers for the empty seats, after a wait.
//!
//! The village deals at five, so one tab is a game that never starts. Bots fill
//! the remaining seats once someone has waited a while, rather than at startup:
//! five people opening five tabs should get each other.
//!
//! They play from [`village_view`], the same payload a browser receives. A bot
//! reading `VillageState` would know who the wolf is.

use std::time::Duration;

use plaza::{
  agent::Agent,
  controller::{query_with, CommandSender, ControllerCommand},
};
use tokio::time::Instant;
use tracing::info;

use crate::snapshot::village_view;
use crate::types::{PlayerId, Role, VillageOp, VillagePhase, VillageState, VillageView, SEATS};

pub type VillageCommands = CommandSender<VillageOp, PlayerId, VillageState>;

/// How long a seat stays open for a person before a bot takes it.
const WAIT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(500);
const LOOK: Duration = Duration::from_millis(250);
/// Long enough that a person can watch the bot act and well inside the night
/// and day clocks so the village is not acting for it.
const THINK: Duration = Duration::from_millis(1500);

/// Seats bots to fill the village once someone has waited, then plays them.
///
/// Seats lock during a night or day, so a bot is only seated while the village
/// is waiting or showing a reveal.
pub async fn fill_the_village(tx: VillageCommands, ids: Vec<PlayerId>) {
  let mut waited = Duration::ZERO;
  let mut seated: Vec<PlayerId> = Vec::new();

  loop {
    tokio::time::sleep(POLL).await;

    let Ok((humans, occupied, open)) = query_with(&tx, |state: &VillageState| {
      let humans = state
        .agents
        .values()
        .filter(|agent| matches!(agent, Agent::Human(_)))
        .count();
      let open = matches!(*state.phase.current(), VillagePhase::Waiting | VillagePhase::Over);
      (humans, state.seats.occupied_count(), open)
    })
    .await
    else {
      return;
    };

    if humans == 0 || occupied >= SEATS || !open {
      waited = Duration::ZERO;
      continue;
    }

    waited += POLL;
    if waited < WAIT {
      continue;
    }
    // Reset, so seats fill one at a time and a late arrival still gets one.
    waited = Duration::ZERO;
    let Some(id) = ids.iter().find(|id| !seated.contains(id)).copied() else {
      continue;
    };
    info!(id, "a seat has been open {}s; seating a bot", WAIT.as_secs());
    if tx
      .send(ControllerCommand::HandleAgentJoined {
        agent: Agent::new_bot(id),
      })
      .await
      .is_err()
    {
      return;
    }
    seated.push(id);
    tokio::spawn(play(tx.clone(), id));
  }
}

/// What a bot does with `view`: the wolf hunts at night and every living bot
/// votes once by day.
pub fn choose(view: &VillageView, me: PlayerId) -> Option<VillageOp> {
  if !view.living.contains(&me) {
    return None;
  }
  match (view.phase, view.your_role?) {
    (VillagePhase::Night, Role::Wolf) => {
      let prey: Vec<PlayerId> = view.living.iter().copied().filter(|p| *p != me).collect();
      prey.get(turn(view) % prey.len().max(1)).copied().map(VillageOp::Hunt)
    }
    (VillagePhase::Day, _) if view.your_vote.is_none() => Some(VillageOp::Vote(suspect(view, me))),
    _ => None,
  }
}

fn turn(view: &VillageView) -> usize {
  (view.games + view.round) as usize
}

/// The same pick for every bot, since `living` is in seat order for everyone.
/// A split vote exiles nobody and that favours the wolf, so knowing nothing
/// the bots at least agree. The suspect votes for the next in line.
fn suspect(view: &VillageView, me: PlayerId) -> PlayerId {
  let living = &view.living;
  let at = turn(view) % living.len();
  if living[at] != me {
    living[at]
  } else {
    living[(at + 1) % living.len()]
  }
}

/// Plays one bot, from what that bot was sent and nothing else.
async fn play(tx: VillageCommands, me: PlayerId) {
  let mut pending: Option<(VillageOp, Instant)> = None;
  loop {
    tokio::time::sleep(LOOK).await;

    let Ok(view) = query_with(&tx, move |state: &VillageState| village_view(state, Some(me))).await else {
      return;
    };
    let Some(op) = choose(&view, me) else {
      pending = None;
      continue;
    };
    match &pending {
      Some((waiting, since)) if *waiting == op => {
        if since.elapsed() < THINK {
          continue;
        }
      }
      _ => {
        pending = Some((op, Instant::now()));
        continue;
      }
    }
    pending = None;
    if tx
      .send(ControllerCommand::SubmitAgentOps {
        agent: Agent::new_bot(me),
        ops: vec![op],
      })
      .await
      .is_err()
    {
      return;
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::guard::VillageGuard;
  use crate::logic::VillageLogic;
  use plaza::op_guard::{OpClearance, OpGuard as _};
  use plaza::state_logic::{LogicInput, StateLogic as _};

  fn view(phase: VillagePhase, role: Option<Role>) -> VillageView {
    VillageView {
      phase,
      round: 1,
      living: vec![1, 901, 902, 903, 904],
      dead: Vec::new(),
      your_role: role,
      voted: Vec::new(),
      your_vote: None,
      everyone: None,
      winner: None,
      wins: Vec::new(),
      games: 1,
    }
  }

  #[test]
  fn the_wolf_hunts_someone_else_alive_at_night() {
    let v = view(VillagePhase::Night, Some(Role::Wolf));
    let Some(VillageOp::Hunt(target)) = choose(&v, 901) else {
      panic!("the wolf should hunt");
    };
    assert_ne!(target, 901);
    assert!(v.living.contains(&target));
  }

  #[test]
  fn a_villager_sleeps_through_the_night() {
    assert_eq!(choose(&view(VillagePhase::Night, Some(Role::Villager)), 901), None);
  }

  #[test]
  fn every_living_bot_votes_once_by_day() {
    for role in [Role::Wolf, Role::Villager] {
      let mut v = view(VillagePhase::Day, Some(role));
      assert!(matches!(choose(&v, 902), Some(VillageOp::Vote(_))), "{role:?} votes");
      v.your_vote = Some(1);
      assert_eq!(choose(&v, 902), None, "{role:?} does not vote twice");
    }
  }

  #[test]
  fn the_bots_agree_on_one_suspect_and_nobody_votes_for_itself() {
    let v = view(VillagePhase::Day, Some(Role::Villager));
    let ballots: Vec<(PlayerId, PlayerId)> = v
      .living
      .iter()
      .map(|me| match choose(&v, *me) {
        Some(VillageOp::Vote(target)) => (*me, target),
        other => panic!("{me} chose {other:?}"),
      })
      .collect();
    assert!(ballots.iter().all(|(me, target)| me != target), "{ballots:?}");

    let suspect = ballots[1].1;
    let agreeing = ballots.iter().filter(|(_, target)| *target == suspect).count();
    assert_eq!(agreeing, v.living.len() - 1, "all but the suspect: {ballots:?}");
  }

  #[test]
  fn the_dead_the_unseated_and_the_idle_phases_do_nothing() {
    let mut dead = view(VillagePhase::Day, Some(Role::Villager));
    dead.living.retain(|p| *p != 903);
    dead.dead.push((903, Role::Villager));
    assert_eq!(choose(&dead, 903), None);

    assert_eq!(choose(&view(VillagePhase::Night, None), 905), None, "a spectator");
    assert_eq!(choose(&view(VillagePhase::Waiting, Some(Role::Wolf)), 901), None);
    assert_eq!(choose(&view(VillagePhase::Over, Some(Role::Villager)), 901), None);
  }

  #[tokio::test]
  async fn bots_alone_play_a_game_to_the_end_without_a_deadline() {
    let mut state = VillageState::new();
    let bots: Vec<PlayerId> = (901..).take(SEATS).collect();
    for id in &bots {
      VillageLogic
        .process_input(&mut state, LogicInput::AgentJoined {
          agent: Agent::new_bot(*id),
        })
        .await
        .unwrap();
    }
    assert_eq!(*state.phase.current(), VillagePhase::Night);

    for _ in 0..20 {
      if *state.phase.current() == VillagePhase::Over {
        break;
      }
      for id in &bots {
        let Some(op) = choose(&village_view(&state, Some(*id)), *id) else {
          continue;
        };
        let source = Agent::new_bot(*id);
        assert_eq!(VillageGuard.guard(&state, &source, &op), OpClearance::Cleared, "{id} chose {op:?}");
        VillageLogic
          .process_input(&mut state, LogicInput::AgentOps { source, ops: vec![op] })
          .await
          .unwrap();
      }
    }
    assert_eq!(*state.phase.current(), VillagePhase::Over);
    assert!(state.winner().is_some());
  }
}
