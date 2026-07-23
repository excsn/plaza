use std::any::Any;
use std::fmt::Debug;
use std::sync::Arc;

use crate::agent::{Agent, AgentId};
pub use crate::error::SnapshotError;
use async_trait::async_trait;

/// Re-exported from [`plaza_wire`]: it travels on the wire, so its definition
/// belongs where a wasm client can reach it.
pub use plaza_wire::envelope::SnapshotData;

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
/// async fn create_snapshot_data(
///   &self, state: &Game, target: Option<&Agent<PlayerId>>, _ctx: Option<SnapshotContext>,
/// ) -> Result<SnapshotData<GameView>, SnapshotError<PlayerId>> {
///   let me = target.and_then(|a| a.id());
///   Ok(SnapshotData { payload: GameView {
///     my_hand: me.and_then(|id| state.hands.get(id)).cloned().unwrap_or_default(),
///     opponent_hand_sizes: state.hands.iter()
///       .filter(|(id, _)| Some(*id) != me)
///       .map(|(id, h)| (id.clone(), h.len()))
///       .collect(),
///   }})
/// }
/// ```
///
/// The controller calls this once per recipient, so returning a different
/// payload per agent costs nothing extra structurally.
#[async_trait]
pub trait SnapshotProvider<ID: AgentId, StateType, SnapshotPayload>: Send + Sync + 'static {
  /// Builds snapshot data from the current authoritative state.
  ///
  /// `target_agent` is `None` only when no particular recipient applies.
  async fn create_snapshot_data(
    &self,
    full_state: &StateType,
    target_agent: Option<&Agent<ID>>,
    context: Option<SnapshotContext>,
  ) -> Result<SnapshotData<SnapshotPayload>, SnapshotError<ID>>;
}
