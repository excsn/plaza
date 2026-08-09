//! A queue with priority bands: better ranks first, arrival order within a
//! band, membership removal. One of the two blocks [`Roster`](super::Roster)
//! is composed of, and public for the same reason: a queueing rule `Roster`
//! does not express is built from this directly.

#[derive(Clone, Debug)]
struct Entry<Key> {
  key: Key,
  rank: u32,
  seq: u64,
}

#[derive(Clone, Debug, Default)]
pub struct RankedQueue<Key: Eq + Clone> {
  entries: Vec<Entry<Key>>,
  next_seq: u64,
}

impl<Key: Eq + Clone> RankedQueue<Key> {
  pub fn new() -> Self {
    Self {
      entries: Vec::new(),
      next_seq: 0,
    }
  }

  /// Queues `key` at `rank` (lower is better), returning its position.
  pub fn push(&mut self, key: Key, rank: u32) -> usize {
    let seq = self.next_seq;
    self.next_seq += 1;
    let at = self
      .entries
      .iter()
      .position(|entry| (entry.rank, entry.seq) > (rank, seq))
      .unwrap_or(self.entries.len());
    self.entries.insert(at, Entry { key, rank, seq });
    at
  }

  pub fn remove(&mut self, key: &Key) -> bool {
    match self.entries.iter().position(|entry| entry.key == *key) {
      Some(at) => {
        self.entries.remove(at);
        true
      }
      None => false,
    }
  }

  pub fn position(&self, key: &Key) -> Option<usize> {
    self.entries.iter().position(|entry| entry.key == *key)
  }

  /// The next key out and its rank, without taking it.
  pub fn best(&self) -> Option<(&Key, u32)> {
    self.entries.first().map(|entry| (&entry.key, entry.rank))
  }

  pub fn pop_best(&mut self) -> Option<(Key, u32)> {
    if self.entries.is_empty() {
      return None;
    }
    let entry = self.entries.remove(0);
    Some((entry.key, entry.rank))
  }

  pub fn iter(&self) -> impl Iterator<Item = &Key> + '_ {
    self.entries.iter().map(|entry| &entry.key)
  }

  pub fn len(&self) -> usize {
    self.entries.len()
  }

  pub fn is_empty(&self) -> bool {
    self.entries.is_empty()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn better_ranks_first_and_arrival_order_within_a_band() {
    let mut queue: RankedQueue<&str> = RankedQueue::new();
    queue.push("bot-a", 1);
    queue.push("person-a", 0);
    queue.push("bot-b", 1);
    queue.push("person-b", 0);

    let order: Vec<&&str> = queue.iter().collect();
    assert_eq!(order, [&"person-a", &"person-b", &"bot-a", &"bot-b"]);
    assert_eq!(queue.best(), Some((&"person-a", 0)));
    assert_eq!(queue.position(&"bot-a"), Some(2));
  }

  #[test]
  fn removal_leaves_the_order_intact() {
    let mut queue: RankedQueue<u64> = RankedQueue::new();
    queue.push(1, 0);
    queue.push(2, 0);
    queue.push(3, 0);
    assert!(queue.remove(&2));
    assert!(!queue.remove(&2));
    assert_eq!(queue.pop_best(), Some((1, 0)));
    assert_eq!(queue.pop_best(), Some((3, 0)));
    assert!(queue.is_empty());
  }

  #[test]
  fn a_requeued_key_goes_to_the_back_of_its_band() {
    let mut queue: RankedQueue<u64> = RankedQueue::new();
    queue.push(1, 1);
    queue.push(2, 1);
    queue.remove(&1);
    queue.push(1, 1);
    assert_eq!(queue.pop_best(), Some((2, 1)));
    assert_eq!(queue.pop_best(), Some((1, 1)));
  }
}
