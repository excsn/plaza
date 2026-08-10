//! Does what the server sent decode into what the server holds?
//!
//! Delta encoding rests on one property: the server's record of what a client
//! holds and the client's own record stay identical, frame after frame. Nothing
//! else checks it. `pack.rs` exercises the encoder against the decoder with one
//! shared harness, and the client's own tests hand-build frames rather than
//! receiving them, so both sides of the real path have been tested against a
//! stand-in and never against each other.
//!
//! If the two baselines ever drift, a delta decodes against the wrong previous
//! value and the yard corrupts silently: no error, no dropped frame, just cubes
//! in the wrong places that look like a physics bug.
//!
//! This drives `YardLogic` directly rather than through a socket, because that
//! is what puts the server's own truth in reach for comparison.

#![cfg(feature = "server")]

use cube_yard::logic::YardLogic;
use cube_yard::pack::{self, Quantized};
use cube_yard::protocol::{CubeState, Cubes, Encoding, PlayerId, YardOp};
use cube_yard::state::YardState;
use plaza::agent::Agent;
use plaza::state_logic::{LogicInput, StateLogic};

/// The decoding half of `NetClient`, kept here so the test exercises the same
/// two functions the client does rather than a paraphrase of them.
#[derive(Default)]
struct Held {
  cubes: Vec<CubeState>,
  baseline: Vec<Option<Quantized>>,
  unreadable: u32,
}

impl Held {
  /// Applies one frame and returns which cubes it named.
  fn apply(&mut self, cubes: &Cubes) -> Vec<usize> {
    let payload = match cubes {
      Cubes::Delta(p) => p,
      other => panic!("expected a delta frame, got {other:?}"),
    };
    let Some(patch) = pack::unpack_delta(payload.as_slice(), &mut self.baseline) else {
      self.unreadable += 1;
      return Vec::new();
    };
    let mut named = Vec::new();
    for (index, cube) in patch {
      let index = index as usize;
      if index >= self.cubes.len() {
        self.cubes.resize(index + 1, cube);
      }
      self.cubes[index] = cube;
      named.push(index);
    }
    named
  }
}

async fn tick(state: &mut YardState) -> Vec<YardOp> {
  let out = YardLogic::new()
    .process_input(state, LogicInput::TimeStep {
      delta_time: std::time::Duration::from_millis(16),
    })
    .await
    .unwrap();
  out.ops.into_iter().flat_map(|t| t.ops).collect()
}

fn agrees(a: &CubeState, b: &CubeState) -> bool {
  let step = pack::position_error();
  (0..3).all(|axis| (a.pos[axis] - b.pos[axis]).abs() <= step * 1.5)
}

#[tokio::test]
async fn a_delta_stream_decodes_into_what_the_server_holds() {
  let mut state = YardState::with(Encoding::Delta, false);
  let mut held = Held::default();

  // Joining gets the whole yard once; everything after it is budgeted.
  let out = YardLogic::new()
    .process_input(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(1 as PlayerId),
    })
    .await
    .unwrap();
  let seed: Vec<YardOp> = out.ops.into_iter().flat_map(|t| t.ops).collect();
  let mut seeded = false;
  for op in &seed {
    if let YardOp::Frame(update) = op {
      held.apply(&update.cubes);
      seeded = true;
    }
  }
  assert!(seeded, "a joiner is handed the yard");
  assert_eq!(held.cubes.len(), state.yard.len(), "and all of it");

  // Then run through the collapse, where cubes are moving and deltas are doing
  // real work rather than repeating an unchanged flag.
  let mut checked = 0usize;
  for _ in 0..300 {
    let ops = tick(&mut state).await;
    let truth = {
      let mut cubes = Vec::new();
      state.yard.snapshot(&mut cubes);
      cubes
    };

    for op in ops {
      let YardOp::Frame(update) = op else { continue };
      for index in held.apply(&update.cubes) {
        // A cube named by this frame must decode to where the server has it
        // *now*. This is the assertion that fails the moment the two baselines
        // disagree, because a delta against a wrong previous value lands
        // somewhere else entirely.
        assert!(
          agrees(&held.cubes[index], &truth[index]),
          "cube {index} decoded to {:?}, server holds {:?}",
          held.cubes[index].pos,
          truth[index].pos
        );
        checked += 1;
      }
    }
  }

  assert_eq!(held.unreadable, 0, "every frame the server sent must decode");
  assert!(checked > 10_000, "only {checked} cubes were actually compared");
}

/// Drop one frame and the yard corrupts. That is not a defect, it is the
/// reason the README says this encoding depends on the transport.
///
/// A delta is measured against what the other end is *known* to hold, and the
/// server learns that from having sent it. Over TCP that inference is sound:
/// what was sent is what arrives, in order. Lose a frame and the server's
/// record is ahead of the client's, every later delta is measured from a value
/// the client never saw, and it decodes to somewhere else with no error raised.
///
/// This test exists as much to prove the assertion above has teeth as to
/// document the dependency: a check that has never failed is weak evidence that
/// it could.
#[tokio::test]
async fn a_dropped_frame_corrupts_the_yard_which_is_why_this_needs_an_ordered_transport() {
  let mut state = YardState::with(Encoding::Delta, false);
  let mut held = Held::default();

  let out = YardLogic::new()
    .process_input(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(1 as PlayerId),
    })
    .await
    .unwrap();
  for op in out.ops.into_iter().flat_map(|t| t.ops) {
    if let YardOp::Frame(update) = op {
      held.apply(&update.cubes);
    }
  }

  let mut worst = 0.0f32;
  for tick_index in 0..300 {
    let ops = tick(&mut state).await;
    // One frame, thrown away, exactly as a datagram transport would.
    if tick_index == 20 {
      continue;
    }
    let truth = {
      let mut cubes = Vec::new();
      state.yard.snapshot(&mut cubes);
      cubes
    };
    for op in ops {
      let YardOp::Frame(update) = op else { continue };
      for index in held.apply(&update.cubes) {
        let d = (0..3)
          .map(|a| (held.cubes[index].pos[a] - truth[index].pos[a]).powi(2))
          .sum::<f32>()
          .sqrt();
        worst = worst.max(d);
      }
    }
  }

  println!("\nafter one dropped frame, worst decoded position error: {worst:.3} units");
  assert!(
    worst > pack::position_error() * 10.0,
    "a lost frame should visibly corrupt a delta stream; worst error was only {worst}"
  );
}

/// A budget means a cube is stale until its turn comes round, so the yard the
/// client holds should converge on the server's once everything has settled and
/// had a turn, not merely stay decodable.
#[tokio::test]
async fn the_whole_yard_converges_once_it_settles() {
  let mut state = YardState::with(Encoding::Delta, false);
  let mut held = Held::default();

  let out = YardLogic::new()
    .process_input(&mut state, LogicInput::AgentJoined {
      agent: Agent::new_human(1 as PlayerId),
    })
    .await
    .unwrap();
  for op in out.ops.into_iter().flat_map(|t| t.ops) {
    if let YardOp::Frame(update) = op {
      held.apply(&update.cubes);
    }
  }

  for _ in 0..1200 {
    for op in tick(&mut state).await {
      if let YardOp::Frame(update) = op {
        held.apply(&update.cubes);
      }
    }
  }

  let mut truth = Vec::new();
  state.yard.snapshot(&mut truth);
  let behind = truth
    .iter()
    .zip(&held.cubes)
    .filter(|(t, h)| !agrees(h, t))
    .count();

  assert_eq!(held.unreadable, 0);
  assert_eq!(
    behind, 0,
    "{behind} of {} cubes never caught up with a settled server",
    truth.len()
  );
}
