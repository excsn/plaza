//! Two simulations behind one seam.
//!
//! [`crate::sim`] is the reference: fixed point, owned outright, and the thing
//! the digest claim was written about. The `rapier` feature adds a second
//! backend running the same rink on a real physics engine, and both are
//! reachable in one build so the same input trace can be pushed through each
//! and measured the same way.
//!
//! The seam is [`Simulate`]. A backend owns integration and contact; the rink's
//! *rules* (half-fencing, the goal mouth, the shot-speed top-up, the carry, the
//! speed cap, the drag, and every bot) are not physics and stay shared.

pub mod fx;
#[cfg(feature = "rapier")]
pub mod rapier;

use plaza_client_utils::rollback::{Frame, RollbackConfig, RollbackSession};

use crate::protocol::Physics;
use crate::sim::{PaddleInput, World, SEATS};

/// A simulation the rink can run.
pub trait Simulate: Clone + std::fmt::Debug + Sized + 'static {
  /// Whether [`Self::view`] carries the whole state.
  ///
  /// When it does, every frame is a complete baseline and a joining client is
  /// whole one tick after arriving. When it does not, the difference is the
  /// part a solver carries between frames (contact manifolds, islands, sleep),
  /// and a client seeded from a view diverges on its first contact, so it has
  /// to be handed a [`Self::snapshot`] instead.
  const VIEW_IS_COMPLETE: bool;

  fn step(&self, inputs: &[PaddleInput]) -> Self;

  /// The wire's and the renderer's view. Lossy for a backend whose state is
  /// richer than four paddles and a puck, so it is never fed back in.
  fn view(&self) -> World;

  /// Over the backend's own state, not over [`Self::view`]: a view quantised
  /// to `Fx` would hide exactly the divergence a digest exists to catch.
  fn digest(&self) -> u64;

  fn seed(view: &World) -> Self;

  fn snapshot(&self) -> Result<Vec<u8>, String>;

  fn restore(bytes: &[u8]) -> Result<Self, String>;

  fn physics() -> Physics;
}

/// [`RollbackSession`] takes a bare `fn` pointer, which a trait method is not.
fn advance<S: Simulate>(state: &S, inputs: &[PaddleInput]) -> S {
  state.step(inputs)
}

/// Whether this build can run what the wire is asking for. `Physics::Rapier`
/// carries the exact rapier build that produced it, because two peers agreeing
/// on "rapier" is not agreement: determinism holds same-version-only.
pub fn supported(physics: Physics) -> bool {
  match physics {
    Physics::Fx => true,
    #[cfg(feature = "rapier")]
    Physics::Rapier { pin } => pin == rapier::PIN,
    #[cfg(not(feature = "rapier"))]
    Physics::Rapier { .. } => false,
  }
}

macro_rules! dispatch {
  ($this:expr, $inner:ident => $body:expr) => {
    match $this {
      Self::Fx($inner) => $body,
      #[cfg(feature = "rapier")]
      Self::Rapier($inner) => $body,
    }
  };
}

/// The authority's simulation state: one world, stepped forward.
#[derive(Clone, Debug)]
pub enum Body {
  Fx(World),
  #[cfg(feature = "rapier")]
  Rapier(rapier::RapierWorld),
}

impl Body {
  /// `None` when this build cannot run that backend.
  pub fn new(physics: Physics) -> Option<Self> {
    if !supported(physics) {
      return None;
    }
    Some(match physics {
      Physics::Fx => Self::Fx(World::new()),
      #[cfg(feature = "rapier")]
      Physics::Rapier { .. } => Self::Rapier(rapier::RapierWorld::seed(&World::new())),
      #[cfg(not(feature = "rapier"))]
      Physics::Rapier { .. } => return None,
    })
  }

  pub fn step(&mut self, inputs: &[PaddleInput]) {
    match self {
      Self::Fx(world) => *world = world.step(inputs),
      #[cfg(feature = "rapier")]
      Self::Rapier(world) => *world = world.step(inputs),
    }
  }

  pub fn view(&self) -> World {
    dispatch!(self, world => world.view())
  }

  pub fn digest(&self) -> u64 {
    dispatch!(self, world => world.digest())
  }

  pub fn physics(&self) -> Physics {
    match self {
      Self::Fx(_) => <World as Simulate>::physics(),
      #[cfg(feature = "rapier")]
      Self::Rapier(_) => <rapier::RapierWorld as Simulate>::physics(),
    }
  }

  /// The bytes a joining client needs, or `None` when the frame it is about to
  /// receive already carries everything.
  pub fn baseline(&self) -> Option<Result<Vec<u8>, String>> {
    match self {
      Self::Fx(_) => None,
      #[cfg(feature = "rapier")]
      Self::Rapier(world) => Some(world.snapshot()),
    }
  }
}

impl Body {
  pub fn seed(physics: Physics, view: &World) -> Option<Self> {
    if !supported(physics) {
      return None;
    }
    Some(match physics {
      Physics::Fx => Self::Fx(World::seed(view)),
      #[cfg(feature = "rapier")]
      Physics::Rapier { .. } => Self::Rapier(rapier::RapierWorld::seed(view)),
      #[cfg(not(feature = "rapier"))]
      Physics::Rapier { .. } => return None,
    })
  }

  pub fn restore(physics: Physics, bytes: &[u8]) -> Option<Self> {
    if !supported(physics) {
      return None;
    }
    Some(match physics {
      Physics::Fx => Self::Fx(World::restore(bytes).ok()?),
      #[cfg(feature = "rapier")]
      Physics::Rapier { .. } => Self::Rapier(rapier::RapierWorld::restore(bytes).ok()?),
      #[cfg(not(feature = "rapier"))]
      Physics::Rapier { .. } => return None,
    })
  }
}

/// The backends this build can run, for a usage line.
pub const NAMES: &str = if cfg!(feature = "rapier") { "fx, rapier" } else { "fx (this build has no rapier)" };

pub fn named(name: &str) -> Result<Physics, String> {
  match name {
    "fx" => Ok(Physics::Fx),
    #[cfg(feature = "rapier")]
    "rapier" => Ok(Physics::Rapier { pin: rapier::PIN }),
    #[cfg(not(feature = "rapier"))]
    "rapier" => Err("this build has no rapier backend; rebuild with --features rapier".to_owned()),
    other => Err(format!("unknown physics {other:?}; expected one of: {NAMES}")),
  }
}

/// The value of `--physics` on a command line, defaulting to the reference
/// backend so the rink runs as it always has unless asked otherwise.
pub fn from_args<I: IntoIterator<Item = String>>(args: I) -> Result<Physics, String> {
  let mut args = args.into_iter().skip_while(|a| a != "--physics");
  match args.nth(1) {
    Some(name) => named(&name),
    None => Ok(Physics::Fx),
  }
}

/// The same command line with `--physics <name>` taken out, for the shared role
/// parser, which rejects an argument it does not know.
pub fn without_physics_arg<I: IntoIterator<Item = String>>(args: I) -> Vec<String> {
  let mut kept = Vec::new();
  let mut args = args.into_iter();
  while let Some(arg) = args.next() {
    if arg == "--physics" {
      args.next();
    } else {
      kept.push(arg);
    }
  }
  kept
}

/// Whether a frame alone can seed a joining client on this backend.
pub fn view_is_complete(physics: Physics) -> bool {
  match physics {
    Physics::Fx => <World as Simulate>::VIEW_IS_COMPLETE,
    #[cfg(feature = "rapier")]
    Physics::Rapier { .. } => <rapier::RapierWorld as Simulate>::VIEW_IS_COMPLETE,
    #[cfg(not(feature = "rapier"))]
    Physics::Rapier { .. } => false,
  }
}

/// A client's rollback session over whichever backend the server is running.
///
/// The two arms cannot be one generic session: [`RollbackSession`] is
/// parameterised by its state type, and the backends do not share one.
pub enum Rink {
  Fx(RollbackSession<World, PaddleInput>),
  #[cfg(feature = "rapier")]
  Rapier(RollbackSession<rapier::RapierWorld, PaddleInput>),
}

impl Rink {
  /// Seeds a session from the first authoritative frame. `None` when this
  /// build cannot run what the server is running.
  pub fn new(physics: Physics, view: &World, config: RollbackConfig) -> Option<Self> {
    if !supported(physics) {
      return None;
    }
    let neutral = vec![PaddleInput::default(); SEATS];
    Some(match physics {
      Physics::Fx => Self::Fx(RollbackSession::new(World::seed(view), neutral, config, advance::<World>)),
      #[cfg(feature = "rapier")]
      Physics::Rapier { .. } => Self::Rapier(RollbackSession::new(
        rapier::RapierWorld::seed(view),
        neutral,
        config,
        advance::<rapier::RapierWorld>,
      )),
      #[cfg(not(feature = "rapier"))]
      Physics::Rapier { .. } => return None,
    })
  }

  /// Seeds a session from a serialized backend state, for a backend a frame
  /// cannot seed. `None` when this build cannot run it or the bytes are not
  /// what they claim to be.
  pub fn from_baseline(physics: Physics, state: &[u8], config: RollbackConfig) -> Option<Self> {
    if !supported(physics) {
      return None;
    }
    let neutral = vec![PaddleInput::default(); SEATS];
    Some(match physics {
      Physics::Fx => Self::Fx(RollbackSession::new(
        World::restore(state).ok()?,
        neutral,
        config,
        advance::<World>,
      )),
      #[cfg(feature = "rapier")]
      Physics::Rapier { .. } => Self::Rapier(RollbackSession::new(
        rapier::RapierWorld::restore(state).ok()?,
        neutral,
        config,
        advance::<rapier::RapierWorld>,
      )),
      #[cfg(not(feature = "rapier"))]
      Physics::Rapier { .. } => return None,
    })
  }

  pub fn current_frame(&self) -> Frame {
    dispatch!(self, session => session.current_frame())
  }

  pub fn queue_local_input(&mut self, player: usize, input: PaddleInput) {
    dispatch!(self, session => session.queue_local_input(player, input))
  }

  pub fn confirm_remote_input(&mut self, player: usize, frame: Frame, input: PaddleInput) {
    dispatch!(self, session => session.confirm_remote_input(player, frame, input))
  }

  pub fn advance_frame(&mut self) {
    dispatch!(self, session => session.advance_frame())
  }

  pub fn rollback_count(&self) -> u64 {
    dispatch!(self, session => session.rollback_count())
  }

  pub fn last_rollback_frames(&self) -> usize {
    dispatch!(self, session => session.last_rollback_frames())
  }

  pub fn prediction_horizon(&self) -> usize {
    dispatch!(self, session => session.prediction_horizon())
  }

  pub fn view(&self) -> World {
    dispatch!(self, session => session.state().view())
  }

  pub fn view_at(&self, frame: Frame) -> Option<World> {
    dispatch!(self, session => session.state_at(frame).map(|s| s.view()))
  }

  pub fn digest_at(&self, frame: Frame) -> Option<u64> {
    dispatch!(self, session => session.state_at(frame).map(|s| s.digest()))
  }
}
