use plaza_wire::{MsgPackCodec, WireCodec};

use super::Simulate;
use crate::protocol::Physics;
use crate::sim::{self, PaddleInput, World};

impl Simulate for World {
  /// The whole state is four paddles, a puck and two scores, and all of it is
  /// already in every frame.
  const VIEW_IS_COMPLETE: bool = true;

  fn step(&self, inputs: &[PaddleInput]) -> Self {
    sim::step(self, inputs)
  }

  fn view(&self) -> World {
    self.clone()
  }

  fn digest(&self) -> u64 {
    sim::digest(self)
  }

  fn seed(view: &World) -> Self {
    view.clone()
  }

  fn snapshot(&self) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    MsgPackCodec.encode_into(self, &mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
  }

  fn restore(bytes: &[u8]) -> Result<Self, String> {
    MsgPackCodec.decode::<Self>(bytes).map_err(|e| e.to_string())
  }

  fn physics() -> Physics {
    Physics::Fx
  }
}
