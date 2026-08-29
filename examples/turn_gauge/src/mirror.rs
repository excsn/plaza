//! The client's half of the audit: a state machine fed every op, deriving who
//! must act next and checking each turn the server opens against it.
//!
//! One implementation on purpose. The desktop window, the wasm page, the
//! scripted run and the logic tests all audit through this type, so "the
//! projection agreed with the server" is one piece of code being right rather
//! than four transcriptions of it.

use crate::order;
use crate::protocol::{BattleView, Effect, GaugeOp, Move, Regime, Unit, UnitId, PROJECT};

/// What one observed op amounted to, where the caller cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Observed {
  /// A turn opened and the projection named the same unit.
  Agreed(UnitId),
  /// A turn opened and the projection named someone else (or nobody).
  Diverged { server: Option<UnitId>, mine: Option<UnitId> },
  /// Anything else.
  Noted,
}

#[derive(Clone, Debug, Default)]
pub struct OrderMirror {
  pub regime: Option<Regime>,
  pub seed: u64,
  pub round: u32,
  pub units: Vec<Unit>,
  /// The standing round's order under initiative; empty under the delay
  /// regime.
  pub order: Vec<UnitId>,
  /// How many turns of `order` have opened, the walk's cursor.
  pub taken: usize,
  pub current: Option<UnitId>,

  pub checked: u64,
  pub diverged: u64,
}

impl OrderMirror {
  pub fn new() -> Self {
    Self::default()
  }

  /// Adopts a snapshot as the baseline. The standing round's order arrives as
  /// data because its rolls read speeds the joiner never saw; derivation takes
  /// over at the next boundary.
  pub fn baseline(&mut self, view: &BattleView) {
    self.regime = Some(view.regime);
    self.seed = view.battle;
    self.round = view.round;
    self.units = view.units.clone();
    self.order = view.order.clone();
    self.current = view.current;
    self.taken = match view.current {
      Some(current) => self.order.iter().position(|id| *id == current).map_or(0, |p| p + 1),
      None => 0,
    };
  }

  /// Who must take the next turn, from state alone.
  pub fn expected(&self) -> Option<UnitId> {
    match self.regime? {
      Regime::Ctb => order::ctb_next(&self.units),
      Regime::Initiative => self
        .order
        .iter()
        .skip(self.taken)
        .find(|id| self.alive(**id))
        .copied(),
    }
  }

  /// The act list for the panel: the current actor, then the next
  /// [`PROJECT`] derived after it.
  pub fn projection(&self) -> Vec<UnitId> {
    let Some(regime) = self.regime else {
      return Vec::new();
    };
    let upcoming = order::project(regime, self.seed, self.round, &self.order, self.taken, &self.units, PROJECT);
    match regime {
      // The gauge still has the current actor at its head until it acts.
      Regime::Ctb => upcoming,
      Regime::Initiative => {
        let mut out = Vec::with_capacity(PROJECT + 1);
        if let Some(current) = self.current.filter(|id| self.alive(*id)) {
          out.push(current);
        }
        out.extend(upcoming);
        out.truncate(PROJECT);
        out
      }
    }
  }

  /// The what-if: the act list as it would stand after `actor` played `mv`.
  pub fn preview(&self, actor: UnitId, mv: Move) -> Vec<UnitId> {
    let Some(regime) = self.regime else {
      return Vec::new();
    };
    order::preview(regime, self.seed, self.round, &self.order, self.taken, &self.units, actor, mv, PROJECT)
  }

  pub fn observe(&mut self, op: &GaugeOp) -> Observed {
    match op {
      GaugeOp::Snapshot(view) => {
        // Only ever a baseline: a mirror already tracking stays on its own
        // derivation, which is the thing being audited.
        if self.regime.is_none() {
          self.baseline(view);
        }
        Observed::Noted
      }

      GaugeOp::BattleStarted { battle, regime, units } => {
        self.regime = Some(*regime);
        self.seed = *battle;
        self.round = 0;
        self.units = units.clone();
        self.order.clear();
        self.taken = 0;
        self.current = None;
        Observed::Noted
      }

      GaugeOp::RoundStarted { round } => {
        self.round = *round;
        self.order = order::initiative_order(self.seed, *round, &self.units);
        self.taken = 0;
        Observed::Noted
      }

      GaugeOp::TurnChanged(notice) => {
        let mine = self.expected();
        let server = notice.new_turn_actor;
        self.checked += 1;
        // Adopt the server's answer either way, so one miss is one count
        // rather than a cascade.
        self.current = server;
        if let Some(at) = server.and_then(|id| self.order.iter().skip(self.taken).position(|o| *o == id)) {
          self.taken += at + 1;
        }
        if server == mine {
          Observed::Agreed(server.expect("an opened turn names its actor"))
        } else {
          self.diverged += 1;
          Observed::Diverged { server, mine }
        }
      }

      GaugeOp::ActionDone { unit, mv, effects, .. } => {
        self.apply(*unit, *mv, effects);
        Observed::Noted
      }

      _ => Observed::Noted,
    }
  }

  fn apply(&mut self, actor: UnitId, mv: Move, effects: &[Effect]) {
    let before: Vec<u32> = {
      let mut speeds = vec![0u32; self.units.len()];
      for unit in &self.units {
        speeds[unit.id as usize] = unit.speed;
      }
      speeds
    };
    for effect in effects {
      if let Some(unit) = self.units.iter_mut().find(|u| u.id == effect.unit) {
        unit.hp = effect.hp;
        unit.shield = effect.shield;
        unit.speed = effect.speed;
        unit.charges = effect.charges;
        unit.alive = effect.alive;
      }
    }
    if self.regime == Some(Regime::Ctb)
      && let Some(now) = self.units.iter().find(|u| u.id == actor).map(|u| u.next_at)
    {
      order::ctb_recharge(&mut self.units, actor, now, mv.time(), &before);
    }
  }

  fn alive(&self, id: UnitId) -> bool {
    self.units.iter().any(|u| u.id == id && u.alive)
  }
}
