//! Everything that crosses the wire.
//!
//! Stage one sends every cube, every tick, at full f32 width, which is the
//! number the rest of this example is measured against. The simulation's own
//! state stays on the server: unlike puck_rink, no client re-simulates here, so
//! only this projection has to travel.

use plaza_wire::Payload;
use serde::{Deserialize, Serialize};

/// The wire format's version, derived at build time from this file.
pub const PROTOCOL: u32 = WIRE_PROTOCOL;

include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));

pub type PlayerId = u32;

pub const TICK_HZ: u64 = 60;

/// How many cubes are in the pile. Fiedler's number, so the bandwidth figures
/// line up with his article.
pub const CUBES: usize = 901;

pub fn frame_to_ms(frame: u64) -> u64 {
  frame * 1000 / TICK_HZ
}

/// One cube as the wire currently carries it: position, orientation, velocity,
/// and whether the solver has put it to sleep.
///
/// Nothing is quantised yet. At 901 cubes and 60Hz this is the baseline the
/// packing stages have to beat, and the fields are exactly the ones Fiedler
/// compresses: 96 bits of position, 128 of orientation, 96 of velocity.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CubeState {
  pub pos: [f32; 3],
  pub rot: [f32; 4],
  pub linvel: [f32; 3],
  pub at_rest: bool,
}

/// How a frame carries the yard.
///
/// Both encodings ride the same op so a server can be switched between them and
/// the difference read off one panel, rather than compared across two runs a
/// week apart.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Cubes {
  /// Stage one: every field at full width, through serde.
  Full(Vec<CubeState>),
  /// Stage two: the hand-written bit layout in `pack`, carried as bytes rather
  /// than as a `Vec<u8>`, which would re-encode every byte as an integer.
  Packed(Payload),
  /// Stage three: only the cubes that fit this tick's budget, each naming
  /// itself. The client patches these into the yard it already holds.
  Subset(Payload),
  /// Stage four: the same, with each cube encoded against what the client is
  /// known to hold. A separate variant rather than a flag because the two
  /// layouts are not distinguishable from their bytes, and guessing wrong
  /// would decode garbage into a baseline both ends have to agree on.
  Delta(Payload),
}

impl Cubes {
  pub fn is_packed(&self) -> bool {
    matches!(self, Self::Packed(_) | Self::Subset(_) | Self::Delta(_))
  }

}

/// One authoritative tick.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameUpdate {
  pub frame: u64,
  pub server_time_ms: u64,
  /// The player cube this client drives, if it has one.
  pub yours: Option<u16>,
  pub cubes: Cubes,
}

/// Which wire encoding the yard is running.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Encoding {
  /// Full width, through serde. The number every stage is measured against.
  #[default]
  Full,
  /// Quantised and bit-packed by hand.
  Packed,
  /// Packed, and only what fits a hard byte budget each tick.
  Budgeted,
  /// Budgeted, and each cube encoded against what the client already holds.
  Delta,
}

impl Encoding {
  pub fn named(name: &str) -> Result<Self, String> {
    match name {
      "full" => Ok(Self::Full),
      "packed" => Ok(Self::Packed),
      "budgeted" => Ok(Self::Budgeted),
      "delta" => Ok(Self::Delta),
      other => Err(format!("unknown encoding {other:?}; expected full, packed, budgeted or delta")),
    }
  }

  /// The value of `--encoding` on a command line.
  pub fn from_args<I: IntoIterator<Item = String>>(args: I) -> Result<Self, String> {
    let mut args = args.into_iter().skip_while(|a| a != "--encoding");
    match args.nth(1) {
      Some(name) => Self::named(&name),
      None => Ok(Self::default()),
    }
  }
}

/// The value of `--send-hz`, defaulting to the tick rate.
pub fn send_hz_from_args<I: IntoIterator<Item = String>>(args: I) -> Result<u64, String> {
  let mut args = args.into_iter().skip_while(|a| a != "--send-hz");
  match args.nth(1) {
    Some(text) => text
      .parse::<u64>()
      .map_err(|_| format!("--send-hz wants a number, got {text:?}"))
      .and_then(|hz| {
        if (1..=TICK_HZ).contains(&hz) {
          Ok(hz)
        } else {
          Err(format!("--send-hz must be between 1 and {TICK_HZ}"))
        }
      }),
    None => Ok(TICK_HZ),
  }
}

/// The same command line with `--encoding <name>` and `--snap` removed, for the
/// shared role parser, which rejects an argument it does not know.
pub fn without_yard_args<I: IntoIterator<Item = String>>(args: I) -> Vec<String> {
  let mut kept = Vec::new();
  let mut args = args.into_iter();
  while let Some(arg) = args.next() {
    match arg.as_str() {
      "--encoding" => {
        args.next();
      }
      "--snap" => {}
      "--send-hz" => {
        args.next();
      }
      _ => kept.push(arg),
    }
  }
  kept
}

/// What a player is holding this tick, in world axes.
///
/// The camera sits at a fixed offset behind the cube rather than orbiting, so
/// "left" means the same direction from one second to the next and these can be
/// world axes without a frame conversion.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Drive {
  pub dx: i8,
  pub dz: i8,
  pub jump: bool,
  /// Which mode the cube is in, as a toggle rather than a press, so a lost
  /// input cannot strand it in the wrong one.
  ///
  /// `false` hovers: the cube floats and shoves the field aside without
  /// touching it. `true` rolls: it drops, tumbles along the ground, and weakly
  /// holds on to whatever it runs into.
  pub rolling: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum YardOp {
  Drive(Drive),
  /// Sent once, to one client, naming the cube it drives.
  Seated { cube: u16 },
  Frame(Box<FrameUpdate>),
}
