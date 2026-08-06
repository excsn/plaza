use std::any::Any;
use std::fmt::Debug;
use std::sync::Arc;

use crate::agent::{Agent, AgentId};
pub use crate::error::SnapshotError;
use async_trait::async_trait;

/// What kind of snapshot is being asked for.
///
/// Plaza never reads this. It travels from whoever asks for a snapshot to your
/// [`SnapshotProvider`], which is free to interpret or ignore it: both ends are
/// yours, and the controller only carries it.
///
/// The named variants are conveniences for common cases, not a vocabulary you
/// are limited to. When your notion of "which snapshot" is anything else, a
/// content hash, a vector clock, a Lamport timestamp, a typed view enum: use
/// [`Custom`](Self::Custom) and downcast on the other side:
///
/// ```ignore
/// #[derive(Clone)]
/// struct SinceDigest([u8; 32]);
///
/// tx.send(ControllerCommand::SendSnapshots {
///   recipients,
///   context: Some(SnapshotContext::custom(SinceDigest(client_digest))),
/// }).await?;
///
/// // In your provider:
/// if let Some(SinceDigest(d)) = context.as_ref().and_then(SnapshotContext::downcast_ref) {
///   return Ok(self.delta_since(state, d));
/// }
/// ```
#[derive(Clone, Default)]
pub enum SnapshotContext {
  /// The whole state.
  #[default]
  Full,
  /// Only what changed since version `u64`.
  ///
  /// A convenience for the common case of a monotonic counter. If your versions
  /// are not `u64`, use [`Custom`](Self::Custom) rather than squeezing them into
  /// one: plaza has no opinion on how you version state, and tracks nothing
  /// itself.
  DeltaFromVersion(u64),
  /// A named view, e.g. `"player"` or `"spectator"`.
  ///
  /// A convenience for the common case of a handful of named perspectives. Use
  /// [`Custom`](Self::Custom) for a typed enum if stringly-typed views bother
  /// you, which is reasonable.
  ForPerspective(String),
  /// Anything else your application means by "which snapshot".
  ///
  /// Build it with [`custom`](Self::custom) and read it with
  /// [`downcast_ref`](Self::downcast_ref).
  Custom(Arc<dyn Any + Send + Sync>),
}

impl SnapshotContext {
  /// Wraps an application-defined context.
  pub fn custom<T: Any + Send + Sync>(value: T) -> Self {
    SnapshotContext::Custom(Arc::new(value))
  }

  /// Reads a [`Custom`](Self::Custom) context back as `T`, or `None` if this is
  /// a different variant or a different type.
  pub fn downcast_ref<T: Any + Send + Sync>(&self) -> Option<&T> {
    match self {
      SnapshotContext::Custom(value) => value.downcast_ref::<T>(),
      _ => None,
    }
  }
}

impl Debug for SnapshotContext {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      SnapshotContext::Full => write!(f, "Full"),
      SnapshotContext::DeltaFromVersion(v) => write!(f, "DeltaFromVersion({v})"),
      SnapshotContext::ForPerspective(name) => write!(f, "ForPerspective({name:?})"),
      // The payload is app-defined and need not be Debug.
      SnapshotContext::Custom(_) => write!(f, "Custom(..)"),
    }
  }
}

/// Produces the state a client is sent.
///
/// This is the seam for hidden information. `target_agent` says who the snapshot
/// is *for*, so one state can yield a different payload per recipient, a card
/// game shows each player their own hand and only the count of everyone else's:
///
/// ```ignore
/// // The snapshot variant is boxed: unboxed, every `Op` in every batch would
/// // be as large as a whole `GameView`.
/// enum GameOp { Play(Card), Snapshot(Box<GameView>) }
///
/// async fn create_snapshot(
///   &self, state: &Game, target: Option<&Agent<PlayerId>>, _ctx: Option<SnapshotContext>,
/// ) -> Result<Option<GameOp>, SnapshotError<PlayerId>> {
///   let me = target.and_then(|a| a.id());
///   Ok(Some(GameOp::Snapshot(Box::new(GameView {
///     my_hand: me.and_then(|id| state.hands.get(id)).cloned().unwrap_or_default(),
///     opponent_hand_sizes: state.hands.iter()
///       .filter(|(id, _)| Some(*id) != me)
///       .map(|(id, h)| (id.clone(), h.len()))
///       .collect(),
///   }))))
/// }
/// ```
///
/// The controller calls this once per recipient, so returning a different
/// payload per agent costs nothing extra structurally. When the payload does
/// not depend on the recipient, a uniform request collapses the pass to one
/// call: see `SnapshotRequest::uniform`.
///
/// **Every call in a pass is started before any is awaited.** A provider that
/// reads a database or a cache therefore overlaps its waits rather than
/// serialising them, which matters because the controller is one task and a
/// pass that awaits per agent stalls ticks and ops behind it. The consequence
/// is that calls interleave: one relying on finishing before the next begins
/// cannot assume it.
#[async_trait]
pub trait SnapshotProvider<ID: AgentId, StateType, Op>: Send + Sync + 'static {
  /// Builds a snapshot of the current authoritative state, as an `Op`.
  ///
  /// A snapshot is an operation like any other: the envelope carries no second
  /// message kind, so "replace everything" is a variant of your `Op` rather
  /// than a wire concept. **Box it** if it carries a whole state view, or every
  /// `Op` in a batch is sized to it.
  ///
  /// Return `Ok(None)` to send this recipient nothing: an application with no
  /// snapshot concept at all, or one declining a particular agent, says so here
  /// rather than inventing an empty op.
  ///
  /// `target_agent` is `None` only when no particular recipient applies. A
  /// uniform pass ([`SnapshotRequest::uniform`]) calls this once with `None`
  /// and sends the result to every recipient in the request, so the `None`
  /// view must contain nothing any recipient may not see.
  ///
  /// [`SnapshotRequest::uniform`]: crate::state_logic::SnapshotRequest::uniform
  async fn create_snapshot(
    &self,
    full_state: &StateType,
    target_agent: Option<&Agent<ID>>,
    context: Option<SnapshotContext>,
  ) -> Result<Option<Op>, SnapshotError<ID>>;
}

/// A [`SnapshotProvider`] that is just a view function.
///
/// Most providers are a pure function of the state and the recipient with an
/// `async fn` and an `Ok(..)` wrapped around it. This is that wrapper, written
/// once: hand it `fn view(state: &S, target: Option<&Agent<ID>>) -> Option<Op>`
/// and it is a provider. Return `None` to send a recipient nothing.
///
/// ```ignore
/// fn view(state: &Game, target: Option<&Agent<PlayerId>>) -> Option<GameOp> {
///   Some(GameOp::Snapshot(Box::new(state.as_seen_by(target))))
/// }
/// let provider = Arc::new(SnapshotFn(view));
/// ```
///
/// A named function coerces cleanly; a closure usually needs its argument
/// types written out. Anything fallible, or anything that must await, still
/// implements [`SnapshotProvider`] directly.
pub struct SnapshotFn<F>(pub F);

#[async_trait]
impl<ID, StateType, Op, F> SnapshotProvider<ID, StateType, Op> for SnapshotFn<F>
where
  ID: AgentId,
  StateType: Send + Sync + 'static,
  Op: Send + 'static,
  F: for<'a> Fn(&'a StateType, Option<&'a Agent<ID>>) -> Option<Op> + Send + Sync + 'static,
{
  async fn create_snapshot(
    &self,
    full_state: &StateType,
    target_agent: Option<&Agent<ID>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<Op>, SnapshotError<ID>> {
    Ok((self.0)(full_state, target_agent))
  }
}

/// A [`SnapshotProvider`] for an application that has no snapshot concept.
///
/// Answers `Ok(None)` for every recipient, which the controller reads as
/// "send this one nothing". Use it when joining carries no catch-up: a chat
/// relay, a pure event log, a game whose clients rebuild from the op stream.
/// [`StateControllerBuilder::without_snapshots`] takes it for you.
///
/// [`StateControllerBuilder::without_snapshots`]: crate::controller::StateControllerBuilder::without_snapshots
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSnapshots;

#[async_trait]
impl<ID, StateType, Op> SnapshotProvider<ID, StateType, Op> for NoSnapshots
where
  ID: AgentId,
  StateType: Send + Sync + 'static,
  Op: Send + 'static,
{
  async fn create_snapshot(
    &self,
    _full_state: &StateType,
    _target_agent: Option<&Agent<ID>>,
    _context: Option<SnapshotContext>,
  ) -> Result<Option<Op>, SnapshotError<ID>> {
    Ok(None)
  }
}
