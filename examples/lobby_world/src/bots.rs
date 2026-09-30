//! Players for an arena that has a person in it and nobody to compete with.
//!
//! A bot takes an open seat once a human has sat in the arena for [`WAIT`]
//! without anyone else arriving, so a person alone gets competition while
//! people who open several tabs get each other first. Quick match seats bots
//! too, for the seats its queue could not fill; both kinds play the same way.
//!
//! They play from [`RoomView`], the same payload a browser receives. A claim is
//! the `RoomOp::Claim` a browser sends.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use plaza::agent::Agent;
use plaza::controller::{query_with, CommandSender, ControllerCommand};
use tracing::{debug, info};

use crate::room::ArenaState;
use crate::types::{PlayerId, RoomOp, RoomView, Seat};

pub type ArenaCommands = CommandSender<RoomOp, PlayerId, ArenaState>;

/// How long a seat stays open for a person before a bot takes it.
pub const WAIT: Duration = Duration::from_secs(10);
const FILL_POLL: Duration = Duration::from_millis(500);

/// The arena ticks at 20 Hz, so looking faster than this sees nothing new.
const LOOK: Duration = Duration::from_millis(100);

/// The fastest a bot reacts to a pot and how much slower it can be. An
/// attentive person clicks well inside the minimum, so they win the pots they
/// are watching for and lose the ones they are not.
pub const REACTION_MIN: Duration = Duration::from_millis(900);
pub const REACTION_SPREAD: Duration = Duration::from_millis(1200);

/// Well clear of the humans' counter, so a bot is recognisable in a log
/// without consulting anything.
const FIRST_BOT_ID: PlayerId = 1_000_000;
static NEXT_BOT: AtomicU64 = AtomicU64::new(FIRST_BOT_ID);

/// Shared by quick match and the filler, so two bots in one arena never collide.
pub fn next_id() -> PlayerId {
  NEXT_BOT.fetch_add(1, Ordering::Relaxed)
}

/// Decides when an arena gets another bot.
#[derive(Debug, Default)]
pub struct Filler {
  waited: Duration,
  last_taken: u32,
}

impl Filler {
  /// True when a bot should take a seat now. `held` counts reservations the
  /// lobby has placed and nobody has dialled yet, since those seats are spoken for.
  pub fn step(&mut self, view: &RoomView, held: u32, elapsed: Duration) -> bool {
    if view.seats_taken != self.last_taken {
      self.last_taken = view.seats_taken;
      self.waited = Duration::ZERO;
    }
    let a_person_is_playing = view.occupants.iter().any(|o| o.seat == Seat::Player && !o.bot);
    let open = view.seats_total.saturating_sub(view.seats_taken + held);
    if !a_person_is_playing || open == 0 {
      self.waited = Duration::ZERO;
      return false;
    }
    self.waited += elapsed;
    if self.waited < WAIT {
      return false;
    }
    self.waited = Duration::ZERO;
    true
  }
}

/// How long this bot takes to react once `claimed` pots have gone in the
/// arena. Varies from one pot to the next, so a person cannot learn the timing.
pub fn reaction(me: PlayerId, claimed: u32) -> Duration {
  // splitmix64's finaliser: spreads adjacent inputs across the whole range.
  let mut x = me ^ (u64::from(claimed) << 32);
  x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
  x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
  x ^= x >> 31;
  REACTION_MIN + Duration::from_millis(x % REACTION_SPREAD.as_millis() as u64)
}

/// Decides when a bot claims.
#[derive(Debug, Default)]
pub struct Reflex {
  noticed: Option<Duration>,
}

impl Reflex {
  /// True when the bot should claim now. `now` is the bot's own clock.
  pub fn step(&mut self, view: &RoomView, me: PlayerId, now: Duration) -> bool {
    if view.your_seat != Some(Seat::Player) || view.pot == 0 {
      self.noticed = None;
      return false;
    }
    let claimed = view.occupants.iter().map(|o| o.claims_here).sum();
    let noticed = *self.noticed.get_or_insert(now);
    if now.saturating_sub(noticed) < reaction(me, claimed) {
      return false;
    }
    self.noticed = None;
    true
  }
}

/// Seats a bot whenever [`Filler::step`] says so and plays it. Ends when the
/// arena's controller does.
pub async fn fill_the_arena(tx: ArenaCommands) {
  let mut filler = Filler::default();
  loop {
    tokio::time::sleep(FILL_POLL).await;

    let Ok((view, held)) = query_with(&tx, |state: &ArenaState| {
      (state.view_for(None), state.reserved.count() as u32)
    })
    .await
    else {
      return;
    };
    if !filler.step(&view, held, FILL_POLL) {
      continue;
    }

    let bot = next_id();
    info!(bot, arena = %view.arena, "A seat has been open {}s; seating a bot.", WAIT.as_secs());
    let reserve = ControllerCommand::SubmitSystemOps {
      source_description: "arena filler".to_string(),
      ops: vec![RoomOp::Reserve { player: bot }],
    };
    let join = ControllerCommand::HandleAgentJoined {
      agent: Agent::new_bot(bot),
    };
    if tx.send(reserve).await.is_err() || tx.send(join).await.is_err() {
      return;
    }
    tokio::spawn(play(tx.clone(), bot));
  }
}

/// Plays one bot from what it was sent and nothing else. Ends when the bot is
/// no longer in the arena or the controller has gone.
pub async fn play(tx: ArenaCommands, me: PlayerId) {
  let mut reflex = Reflex::default();
  let mut now = Duration::ZERO;
  let mut ticker = tokio::time::interval(LOOK);
  loop {
    ticker.tick().await;
    now += LOOK;

    let Ok(view) = query_with(&tx, move |state: &ArenaState| state.view_for(Some(&me))).await else {
      return;
    };
    if view.your_seat.is_none() {
      debug!(bot = me, arena = %view.arena, "No longer in the arena; bot stopping.");
      return;
    }
    if !reflex.step(&view, me, now) {
      continue;
    }
    if tx
      .send(ControllerCommand::SubmitAgentOps {
        agent: Agent::new_bot(me),
        ops: vec![RoomOp::Claim],
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
  use crate::types::Occupant;

  const BOT: PlayerId = 1_000_000;

  fn occupant(player: PlayerId, seat: Seat, bot: bool) -> Occupant {
    Occupant {
      player,
      seat,
      bot,
      coins: 0,
      claims_here: 0,
    }
  }

  fn view(occupants: Vec<Occupant>, seats_total: u32, pot: u64, your_seat: Option<Seat>) -> RoomView {
    RoomView {
      arena: "test".into(),
      budget_ms: None,
      pot,
      seats_taken: occupants.iter().filter(|o| o.seat == Seat::Player).count() as u32,
      seats_total,
      spectators: occupants.iter().filter(|o| o.seat == Seat::Spectator).count() as u32,
      bots: occupants.iter().filter(|o| o.bot).count() as u32,
      occupants,
      your_seat,
    }
  }

  fn lone_human() -> RoomView {
    view(vec![occupant(1, Seat::Player, false)], 3, 5, None)
  }

  fn seconds(filler: &mut Filler, view: &RoomView, held: u32, secs: u64) -> Vec<bool> {
    (0..secs * 2).map(|_| filler.step(view, held, Duration::from_millis(500))).collect()
  }

  #[test]
  fn a_lone_human_gets_a_bot_after_the_wait_and_not_before() {
    let mut filler = Filler::default();
    let early = seconds(&mut filler, &lone_human(), 0, WAIT.as_secs() - 1);
    assert!(early.iter().all(|seated| !seated), "still waiting for a person");
    let late = seconds(&mut filler, &lone_human(), 0, 1);
    assert!(late.contains(&true), "the wait is over");
  }

  #[test]
  fn an_arena_without_a_seated_human_gets_no_bot() {
    let mut filler = Filler::default();
    let empty = view(vec![], 3, 5, None);
    assert!(seconds(&mut filler, &empty, 0, 60).iter().all(|seated| !seated));

    let watched = view(vec![occupant(1, Seat::Spectator, false)], 3, 5, None);
    assert!(
      seconds(&mut filler, &watched, 0, 60).iter().all(|seated| !seated),
      "a spectator is not someone to compete with"
    );
  }

  #[test]
  fn a_full_arena_gets_no_bot() {
    let mut filler = Filler::default();
    let full = view(vec![occupant(1, Seat::Player, false), occupant(2, Seat::Player, false)], 2, 5, None);
    assert!(seconds(&mut filler, &full, 0, 60).iter().all(|seated| !seated));
  }

  #[test]
  fn a_seat_reserved_for_a_person_is_not_given_to_a_bot() {
    let mut filler = Filler::default();
    let one_open = view(vec![occupant(1, Seat::Player, false)], 2, 5, None);
    assert!(seconds(&mut filler, &one_open, 1, 60).iter().all(|seated| !seated));
  }

  #[test]
  fn an_arrival_restarts_the_wait() {
    let mut filler = Filler::default();
    seconds(&mut filler, &lone_human(), 0, WAIT.as_secs() - 2);
    let two = view(vec![occupant(1, Seat::Player, false), occupant(2, Seat::Player, false)], 3, 5, None);
    let after = seconds(&mut filler, &two, 0, WAIT.as_secs() - 1);
    assert!(after.iter().all(|seated| !seated), "the newcomer gets a full wait too");
  }

  #[test]
  fn seats_fill_one_at_a_time() {
    let mut filler = Filler::default();
    let seated = seconds(&mut filler, &lone_human(), 0, WAIT.as_secs());
    assert_eq!(seated.iter().filter(|s| **s).count(), 1);
    assert_eq!(seated.last(), Some(&true), "seated when the wait ran out");
    // The arena has not shown the new bot yet, so another look must not seat a second.
    assert!(!filler.step(&lone_human(), 0, Duration::from_millis(500)));
  }

  #[test]
  fn reactions_stay_in_range_and_vary() {
    let draws: Vec<Duration> = (0..20).map(|claims| reaction(BOT, claims)).collect();
    for draw in &draws {
      assert!(*draw >= REACTION_MIN && *draw < REACTION_MIN + REACTION_SPREAD, "{draw:?}");
    }
    assert!(draws.iter().any(|d| *d != draws[0]), "not the same delay every pot");
    assert_ne!(reaction(BOT, 0), reaction(BOT + 1, 0), "bots in one arena differ");
  }

  fn seated_bot(pot: u64) -> RoomView {
    view(
      vec![occupant(1, Seat::Player, false), occupant(BOT, Seat::Player, true)],
      3,
      pot,
      Some(Seat::Player),
    )
  }

  #[test]
  fn a_bot_waits_its_reaction_time_before_claiming() {
    let mut reflex = Reflex::default();
    let delay = reaction(BOT, 0);
    let start = Duration::from_secs(5);
    assert!(!reflex.step(&seated_bot(10), BOT, start), "has only just seen it");
    assert!(!reflex.step(&seated_bot(10), BOT, start + delay - Duration::from_millis(1)));
    assert!(reflex.step(&seated_bot(15), BOT, start + delay), "a refill does not restart the reaction");
  }

  #[test]
  fn a_bot_never_claims_an_empty_pot() {
    let mut reflex = Reflex::default();
    for secs in 0..30 {
      assert!(!reflex.step(&seated_bot(0), BOT, Duration::from_secs(secs)));
    }
  }

  #[test]
  fn a_pot_someone_else_took_resets_the_reaction() {
    let mut reflex = Reflex::default();
    let delay = reaction(BOT, 0);
    reflex.step(&seated_bot(10), BOT, Duration::ZERO);
    reflex.step(&seated_bot(0), BOT, delay / 2);
    assert!(!reflex.step(&seated_bot(5), BOT, delay), "a fresh pot is noticed afresh");
    assert!(reflex.step(&seated_bot(5), BOT, delay + delay));
  }

  #[test]
  fn a_bot_claims_once_per_pot() {
    let mut reflex = Reflex::default();
    let delay = reaction(BOT, 0);
    reflex.step(&seated_bot(10), BOT, Duration::ZERO);
    assert!(reflex.step(&seated_bot(10), BOT, delay));
    assert!(!reflex.step(&seated_bot(10), BOT, delay + Duration::from_millis(100)), "the claim is in flight");
  }

  #[test]
  fn a_bot_without_a_seat_never_claims() {
    let mut reflex = Reflex::default();
    let watching = view(vec![occupant(BOT, Seat::Spectator, true)], 3, 10, Some(Seat::Spectator));
    for secs in 0..30 {
      assert!(!reflex.step(&watching, BOT, Duration::from_secs(secs)));
    }
  }
}
