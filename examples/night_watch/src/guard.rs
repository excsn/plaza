//! Checks whether a player may act at all, in this phase and in this role.
//!
//! The controller runs it ahead of [`VillageLogic`]: seat, liveness, phase and
//! role are checked for every act before any handler touches state. Target
//! validity stays in the rules, because it depends on what the act names and
//! not on who sent it.
//!
//! [`VillageLogic`]: crate::logic::VillageLogic

use plaza::agent::Agent;
use plaza::op_guard::{OpClearance, OpGuard};
use tracing::warn;

use crate::types::{PlayerId, Refusal, Role, VillageOp, VillagePhase, VillageState};

#[derive(Debug)]
pub struct VillageGuard;

impl OpGuard<VillageOp, PlayerId, VillageState> for VillageGuard {
  fn guard(&self, state: &VillageState, source: &Agent<PlayerId>, op: &VillageOp) -> OpClearance<VillageOp> {
    let (phase, role) = match op {
      VillageOp::Hunt(_) => (VillagePhase::Night, Some(Role::Wolf)),
      VillageOp::Vote(_) => (VillagePhase::Day, None),
      _ => return OpClearance::Cleared,
    };
    let Some(player) = source.id_cloned() else {
      // An unidentified agent is the logic's InvalidOperation, not a refusal.
      return OpClearance::Cleared;
    };

    let why = if state.seats.seat_of(&player).is_none() {
      Refusal::Spectating
    } else if state.is_dead(player) {
      Refusal::Dead
    } else if *state.phase.current() != phase {
      Refusal::NotNow
    } else if role.is_some_and(|required| state.roles.get(&player) != Some(&required)) {
      Refusal::NotYourRole
    } else {
      return OpClearance::Cleared;
    };
    warn!(player, ?why, "act refused");
    OpClearance::Refused {
      reply: Some(VillageOp::Refused(why)),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_refusal_carries_its_reason_as_the_reply() {
    let state = VillageState::new();
    let verdict = VillageGuard.guard(&state, &Agent::new_human(1), &VillageOp::Vote(2));
    assert_eq!(
      verdict,
      OpClearance::Refused {
        reply: Some(VillageOp::Refused(Refusal::Spectating))
      }
    );
  }

  #[test]
  fn ops_with_no_standing_requirement_are_cleared() {
    let state = VillageState::new();
    let verdict = VillageGuard.guard(&state, &Agent::new_human(1), &VillageOp::YouAre(1));
    assert_eq!(verdict, OpClearance::Cleared);
  }
}
