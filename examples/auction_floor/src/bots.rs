//! Bidders for a floor with one person on it, after a wait.
//!
//! One tab never contests anything. Bots join one at a time once someone has
//! been bidding with fewer than [`BIDDERS`] on the floor for a while, rather
//! than at startup: several people opening several tabs should get each other.
//!
//! They bid from [`frame_for`], the same `Frame` a browser receives. They name
//! ticks by the page's rule, so the server arbitrates their claims like anyone
//! else's.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use plaza::{
  agent::Agent,
  controller::{query_with, CommandSender, ControllerCommand},
};
use tracing::info;

use crate::logic::{frame_for, Floor, FloorSession};
use crate::types::{AuctionOp, FloorView, ItemId, PlayerId, Tick, TICK_HZ};

pub type FloorCommands = CommandSender<AuctionOp, PlayerId, Floor>;

/// Bots join until this many are bidding.
pub const BIDDERS: usize = 3;
/// How long someone bids without enough company before a bot joins.
const WAIT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(500);

/// Every reaction lands between 300 and 440 ms, inside the window.
const REACTION_BASE_MS: u64 = 300;
const REACTION_PACE_MS: u64 = 40;
const REACTION_SPREAD_MS: u64 = 100;

/// The ticks after a drop before bot `me` reacts to `item`: a pace of its own
/// plus a spread fixed per item, rounded up.
pub fn reaction_ticks(me: PlayerId, item: ItemId) -> Tick {
  let pace = (me % 2) * REACTION_PACE_MS;
  let mix = me.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (item as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
  let spread = (mix >> 32) % (REACTION_SPREAD_MS + 1);
  let ms = REACTION_BASE_MS + pace + spread;
  (ms * TICK_HZ as u64).div_ceil(1000)
}

/// The claims bot `view.you` makes on this frame, as `(item, named tick)`.
///
/// The named tick is the page's: the frame's tick, never under `your_floor`.
/// The bot reads a fresh frame every poll, so the page's extrapolation
/// between frames is zero here.
pub fn choose(view: &FloorView, claimed: &HashSet<ItemId>) -> Vec<(ItemId, Tick)> {
  view
    .items
    .iter()
    .filter(|item| !claimed.contains(&item.id))
    .filter(|item| view.tick >= item.dropped_at + reaction_ticks(view.you, item.id))
    .filter_map(|item| {
      let named = view.tick.max(item.dropped_at + view.your_floor);
      (named <= item.dropped_at + view.window).then_some((item.id, named))
    })
    .collect()
}

/// Joins bots once someone has bid without enough company, then bids for them.
pub async fn fill_the_floor(tx: FloorCommands, session: Arc<FloorSession>, ids: Vec<PlayerId>) {
  let mut waited = Duration::ZERO;
  let mut joined: Vec<PlayerId> = Vec::new();

  loop {
    tokio::time::sleep(POLL).await;

    let Ok((humans, bidders)) = query_with(&tx, |state: &Floor| {
      let humans = state
        .players
        .iter()
        .filter(|(_, info)| matches!(info.agent, Agent::Human(_)))
        .count();
      (humans, state.players.iter().count())
    })
    .await
    else {
      return;
    };

    if humans == 0 || bidders >= BIDDERS {
      waited = Duration::ZERO;
      continue;
    }

    waited += POLL;
    if waited < WAIT {
      continue;
    }
    // Reset, so bots join one at a time.
    waited = Duration::ZERO;
    let Some(id) = ids.iter().find(|id| !joined.contains(id)).copied() else {
      continue;
    };
    info!(id, "a bidder has waited {}s; a bot joins", WAIT.as_secs());
    if tx
      .send(ControllerCommand::HandleAgentJoined {
        agent: Agent::new_bot(id),
      })
      .await
      .is_err()
    {
      return;
    }
    joined.push(id);
    tokio::spawn(bid(tx.clone(), session.clone(), id));
  }
}

/// Bids for one bot, from the frame it is sent and nothing else.
async fn bid(tx: FloorCommands, session: Arc<FloorSession>, me: PlayerId) {
  let mut ticker = tokio::time::interval(Duration::from_millis(1000 / TICK_HZ as u64));
  let mut claimed: HashSet<ItemId> = HashSet::new();
  let mut next_req: u64 = 1;
  loop {
    ticker.tick().await;

    let session = session.clone();
    let Ok(view) = query_with(&tx, move |state: &Floor| frame_for(&session, state, Some(&me))).await else {
      return;
    };
    claimed.retain(|id| view.items.iter().any(|item| item.id == *id));

    let mut ops = Vec::new();
    for (item, tick) in choose(&view, &claimed) {
      claimed.insert(item);
      ops.push(AuctionOp::Grab { req: next_req, item, tick });
      next_req += 1;
    }
    if ops.is_empty() {
      continue;
    }
    if tx
      .send(ControllerCommand::SubmitAgentOps {
        agent: Agent::new_bot(me),
        ops,
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
  use crate::types::{Item, WINDOW};

  const DROP: Tick = 100;

  fn view(me: PlayerId, tick: Tick, your_floor: Tick) -> FloorView {
    FloorView {
      tick,
      window: WINDOW,
      items: vec![Item {
        id: 3,
        value: 50,
        dropped_at: DROP,
        lane: 3,
      }],
      standings: Vec::new(),
      your_floor,
      your_rtt_ms: 0,
      you: me,
    }
  }

  fn ms(ticks: Tick) -> u64 {
    ticks * 1000 / TICK_HZ as u64
  }

  #[test]
  fn a_bot_claims_nothing_before_its_reaction() {
    let react = reaction_ticks(901, 3);
    for tick in DROP..DROP + react {
      assert!(choose(&view(901, tick, 0), &HashSet::new()).is_empty(), "claimed at tick {tick}");
    }
  }

  #[test]
  fn a_bot_names_the_tick_it_reacted_on() {
    let at = DROP + reaction_ticks(901, 3);
    assert_eq!(choose(&view(901, at, 0), &HashSet::new()), vec![(3, at)]);
  }

  #[test]
  fn a_bot_never_names_a_tick_under_its_floor() {
    let react = reaction_ticks(901, 3);
    let floor = react + 1;
    let named = choose(&view(901, DROP + react, floor), &HashSet::new());
    assert_eq!(named, vec![(3, DROP + floor)]);
  }

  #[test]
  fn a_bot_claims_an_item_once() {
    let at = DROP + reaction_ticks(901, 3);
    assert!(choose(&view(901, at, 0), &HashSet::from([3])).is_empty());
  }

  #[test]
  fn a_bot_does_not_claim_after_the_window_closes() {
    assert!(choose(&view(901, DROP + WINDOW + 1, 0), &HashSet::new()).is_empty());
  }

  /// A person clicking in a quarter of a second beats every bot. A bot always
  /// reacts inside the window.
  #[test]
  fn reactions_are_human_scale_and_vary() {
    let mut total = [0, 0];
    let mut paces = HashSet::new();
    for (slot, me) in [901, 902].into_iter().enumerate() {
      for item in 0..64 {
        let react = reaction_ticks(me, item);
        assert!(ms(react) > 250, "bot {me} item {item}: {} ms", ms(react));
        assert!(react <= WINDOW, "bot {me} item {item}: {react} ticks");
        total[slot] += react;
        if me == 901 {
          paces.insert(react);
        }
      }
    }
    assert_ne!(total[0], total[1], "each bot has its own pace");
    assert!(paces.len() > 1, "a bot's reaction varies per item");
  }
}
