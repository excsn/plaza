use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CounterUser(pub u32);

impl fmt::Display for CounterUser {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "User({})", self.0)
  }
}
pub type CounterId = CounterUser;

#[derive(Clone, Debug, Default, Serialize, Deserialize)] // Default for initial state
pub struct CounterStateData {
  pub value: i64,
  pub version: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum CounterOp {
  Increment(i64),
  Set(i64),
}

pub type CounterSnapshotPayload = CounterStateData; // Snapshot is the full state data
