//! Cards, the seeded deal, and the seven-card evaluator. Shared so a client
//! could rank a showdown itself; the server's word still settles it.

/// 0..52: rank `card % 13` (0 is the deuce, 12 the ace), suit `card / 13`.
pub type Card = u8;

pub fn rank(card: Card) -> u8 {
  card % 13
}

pub fn suit(card: Card) -> u8 {
  card / 13
}

pub fn rank_name(card: Card) -> &'static str {
  ["2", "3", "4", "5", "6", "7", "8", "9", "10", "J", "Q", "K", "A"][rank(card) as usize]
}

pub fn suit_name(card: Card) -> &'static str {
  ["♣", "♦", "♥", "♠"][suit(card) as usize]
}

fn rng(seed: u64) -> u64 {
  plaza_client_utils::determinism::mix64(seed)
}

/// A full deck, Fisher-Yates over the hand's seed: the same hand deals the
/// same cards on any machine.
pub fn shuffled(seed: u64) -> Vec<Card> {
  let mut deck: Vec<Card> = (0..52).collect();
  let mut state = seed;
  for i in (1..deck.len()).rev() {
    state = rng(state);
    deck.swap(i, (state % (i as u64 + 1)) as usize);
  }
  deck
}

/// A hand's worth, ordered: category first, then the five deciding ranks.
/// Categories: 8 straight flush, 7 quads, 6 full house, 5 flush, 4 straight,
/// 3 trips, 2 two pair, 1 pair, 0 high card.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct HandRank(pub u8, pub [u8; 5]);

/// The best five-of-seven. Small inputs, exhaustive logic, no cleverness.
pub fn eval7(cards: &[Card]) -> HandRank {
  debug_assert!(cards.len() >= 5 && cards.len() <= 7);

  let mut by_rank = [0u8; 13];
  let mut by_suit = [0u8; 4];
  for card in cards {
    by_rank[rank(*card) as usize] += 1;
    by_suit[suit(*card) as usize] += 1;
  }

  // Flush, and straight flush inside it.
  let flush_suit = (0..4u8).find(|s| by_suit[*s as usize] >= 5);
  if let Some(s) = flush_suit {
    let mut ranks: Vec<u8> = cards.iter().filter(|c| suit(**c) == s).map(|c| rank(*c)).collect();
    ranks.sort_unstable_by(|a, b| b.cmp(a));
    if let Some(high) = straight_high(&ranks) {
      return HandRank(8, [high, 0, 0, 0, 0]);
    }
    return HandRank(5, [ranks[0], ranks[1], ranks[2], ranks[3], ranks[4]]);
  }

  // Groups: quads, boats, trips, pairs.
  let mut groups: Vec<(u8, u8)> = (0..13u8).filter(|r| by_rank[*r as usize] > 0).map(|r| (by_rank[r as usize], r)).collect();
  groups.sort_unstable_by(|a, b| b.cmp(a));
  let kickers = |skip: &[u8], n: usize| -> Vec<u8> {
    let mut out: Vec<u8> = (0..13u8).rev().filter(|r| by_rank[*r as usize] > 0 && !skip.contains(r)).collect();
    out.truncate(n);
    out
  };

  match groups[0] {
    (4, r) => {
      let k = kickers(&[r], 1);
      return HandRank(7, [r, k[0], 0, 0, 0]);
    }
    (3, r) => {
      if let Some((count, pair)) = groups.get(1).copied().filter(|(c, _)| *c >= 2) {
        let _ = count;
        return HandRank(6, [r, pair, 0, 0, 0]);
      }
      let mut all: Vec<u8> = (0..13u8).rev().flat_map(|x| std::iter::repeat_n(x, by_rank[x as usize] as usize)).collect();
      all.sort_unstable_by(|a, b| b.cmp(a));
      if let Some(high) = straight_high(&all) {
        return HandRank(4, [high, 0, 0, 0, 0]);
      }
      let k = kickers(&[r], 2);
      return HandRank(3, [r, k[0], k[1], 0, 0]);
    }
    _ => {}
  }

  let mut all: Vec<u8> = (0..13u8).rev().filter(|r| by_rank[*r as usize] > 0).collect();
  all.sort_unstable_by(|a, b| b.cmp(a));
  if let Some(high) = straight_high(&all) {
    return HandRank(4, [high, 0, 0, 0, 0]);
  }

  let pairs: Vec<u8> = groups.iter().filter(|(c, _)| *c == 2).map(|(_, r)| *r).collect();
  match pairs.as_slice() {
    [a, b, ..] => {
      let k = kickers(&[*a, *b], 1);
      HandRank(2, [*a, *b, k[0], 0, 0])
    }
    [a] => {
      let k = kickers(&[*a], 3);
      HandRank(1, [*a, k[0], k[1], k[2], 0])
    }
    [] => {
      let k = kickers(&[], 5);
      HandRank(0, [k[0], k[1], k[2], k[3], k[4]])
    }
  }
}

/// The high card of the best straight in `ranks` (descending, duplicates
/// tolerated), the wheel included, or `None`.
fn straight_high(ranks: &[u8]) -> Option<u8> {
  let mut present = [false; 14];
  for r in ranks {
    present[*r as usize + 1] = true;
  }
  // The ace plays low below the deuce.
  present[0] = present[13];
  (4..14u8).rev().find(|hi| (0..5).all(|i| present[(*hi - i) as usize]))
    .map(|hi| hi - 1)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn c(rank: u8, suit: u8) -> Card {
    suit * 13 + rank
  }

  #[test]
  fn the_categories_order_themselves() {
    let straight_flush = eval7(&[c(4, 0), c(5, 0), c(6, 0), c(7, 0), c(8, 0), c(0, 1), c(1, 2)]);
    let quads = eval7(&[c(9, 0), c(9, 1), c(9, 2), c(9, 3), c(3, 0), c(4, 1), c(5, 2)]);
    let boat = eval7(&[c(9, 0), c(9, 1), c(9, 2), c(4, 0), c(4, 1), c(2, 2), c(3, 3)]);
    let flush = eval7(&[c(1, 2), c(3, 2), c(5, 2), c(7, 2), c(9, 2), c(0, 0), c(2, 1)]);
    let straight = eval7(&[c(2, 0), c(3, 1), c(4, 2), c(5, 3), c(6, 0), c(9, 1), c(11, 2)]);
    let trips = eval7(&[c(6, 0), c(6, 1), c(6, 2), c(1, 3), c(3, 0), c(8, 1), c(11, 2)]);
    let two_pair = eval7(&[c(6, 0), c(6, 1), c(8, 2), c(8, 3), c(3, 0), c(4, 1), c(11, 2)]);
    let pair = eval7(&[c(6, 0), c(6, 1), c(2, 2), c(8, 3), c(3, 0), c(4, 1), c(11, 2)]);
    let high = eval7(&[c(0, 0), c(2, 1), c(4, 2), c(6, 3), c(8, 0), c(10, 1), c(12, 2)]);

    let mut ladder = [straight_flush, quads, boat, flush, straight, trips, two_pair, pair, high];
    let sorted = {
      let mut s = ladder;
      s.sort_unstable_by(|a, b| b.cmp(a));
      s
    };
    ladder.sort_unstable_by(|a, b| b.cmp(a));
    assert_eq!(ladder, sorted);
    assert_eq!(straight_flush.0, 8);
    assert_eq!(high.0, 0);
  }

  #[test]
  fn the_wheel_is_a_five_high_straight() {
    let wheel = eval7(&[c(12, 0), c(0, 1), c(1, 2), c(2, 3), c(3, 0), c(8, 1), c(10, 2)]);
    assert_eq!(wheel, HandRank(4, [3, 0, 0, 0, 0]), "A-2-3-4-5, the ace playing low");
    let six_high = eval7(&[c(0, 1), c(1, 2), c(2, 3), c(3, 0), c(4, 1), c(8, 1), c(10, 2)]);
    assert!(six_high > wheel);
  }

  #[test]
  fn kickers_break_ties_and_suits_never_do() {
    let kings_ace = eval7(&[c(11, 0), c(11, 1), c(12, 2), c(3, 3), c(5, 0), c(7, 1), c(8, 2)]);
    let kings_ten = eval7(&[c(11, 2), c(11, 3), c(8, 0), c(3, 1), c(5, 2), c(7, 3), c(2, 0)]);
    assert!(kings_ace > kings_ten);

    let hearts = eval7(&[c(1, 2), c(3, 2), c(5, 2), c(7, 2), c(9, 2), c(0, 0), c(2, 1)]);
    let spades = eval7(&[c(1, 3), c(3, 3), c(5, 3), c(7, 3), c(9, 3), c(0, 0), c(2, 1)]);
    assert_eq!(hearts, spades, "a flush is its ranks, not its suit");
  }

  #[test]
  fn the_deal_is_seeded_and_complete() {
    let a = shuffled(7);
    let b = shuffled(7);
    assert_eq!(a, b, "the same seed deals the same deck");
    let mut sorted = a.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, (0..52).collect::<Vec<_>>(), "every card exactly once");
    assert_ne!(a, shuffled(8));
  }
}
