use std::any::Any;
use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::Hash;

use crate::agent::AgentId;
use crate::session::TargetedOp;

/// Defines the interface for the context passed to FSM state methods.
pub trait FsmContext<Op, AppID: AgentId> {
  fn ops_q(&mut self) -> &mut Vec<TargetedOp<Op, AppID>>;
  fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// Trait defining the behavior of a single state in a Finite State Machine.
/// Op, AppID, and StateIdEnum must be 'static because the State trait object is 'static.
pub trait State<
    Op: 'static, // Added 'static bound
    AppID: AgentId, // AgentId already implies 'static
    StateIdEnum: Clone + Debug + PartialEq + Send + 'static, // Added Send + 'static
>: Debug + Send + 'static
{
    fn id(&self) -> StateIdEnum;

    fn on_enter(
        &mut self,
        context: &mut dyn FsmContext<Op, AppID>,
        previous_state_id: Option<StateIdEnum>,
    );

    fn on_update(
        &mut self,
        context: &mut dyn FsmContext<Op, AppID>,
    ) -> Option<StateIdEnum>;

    fn on_exit(
        &mut self,
        context: &mut dyn FsmContext<Op, AppID>,
        next_state_id: &StateIdEnum,
    );
}

/// Manages a collection of states and transitions.
pub struct StateMachine<
  Op: 'static, // Added 'static bound
  AppID: AgentId,
  StateIdEnum: Clone + Debug + PartialEq + Eq + Hash + Send + 'static, // Added Send + 'static
> {
  states: HashMap<StateIdEnum, Box<dyn State<Op, AppID, StateIdEnum>>>,
  current_state_id: Option<StateIdEnum>,
  _phantom: std::marker::PhantomData<(Op, AppID)>,
}

impl<Op: 'static, AppID: AgentId, StateIdEnum: Clone + Debug + PartialEq + Eq + Hash + Send + 'static> Debug
  for StateMachine<Op, AppID, StateIdEnum>
{
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("StateMachine")
      .field("states_count", &self.states.len())
      .field("current_state_id", &self.current_state_id)
      .finish()
  }
}

impl<Op: 'static, AppID: AgentId, StateIdEnum: Clone + Debug + PartialEq + Eq + Hash + Send + 'static> Default
  for StateMachine<Op, AppID, StateIdEnum>
{
  fn default() -> Self {
    Self::new()
  }
}

impl<Op: 'static, AppID: AgentId, StateIdEnum: Clone + Debug + PartialEq + Eq + Hash + Send + 'static>
  StateMachine<Op, AppID, StateIdEnum>
{
  pub fn new() -> Self {
    StateMachine {
      states: HashMap::new(),
      current_state_id: None,
      _phantom: std::marker::PhantomData,
    }
  }

  pub fn add_state(&mut self, state_impl: Box<dyn State<Op, AppID, StateIdEnum>>) {
    self.states.insert(state_impl.id(), state_impl);
  }

  pub fn set_initial_state(
    &mut self,
    context: &mut dyn FsmContext<Op, AppID>, // Op and AppID here are constrained by impl block
    state_id: StateIdEnum,
  ) {
    if !self.states.contains_key(&state_id) {
      panic!("Initial state ID {:?} not added.", state_id);
    }
    if let Some(current_id_clone) = self.current_state_id.clone() {
      if let Some(current_s_impl) = self.states.get_mut(&current_id_clone) {
        current_s_impl.on_exit(context, &state_id);
      }
    }
    self.current_state_id = Some(state_id.clone());
    let initial_s_impl = self
      .states
      .get_mut(&state_id)
      .expect("FSM logic error: State checked for existence but not found for on_enter.");
    initial_s_impl.on_enter(context, None);
  }

  pub fn current_state_id(&self) -> Option<&StateIdEnum> {
    self.current_state_id.as_ref()
  }

  pub fn update(&mut self, context: &mut dyn FsmContext<Op, AppID>) {
    let mut next_state_id_opt: Option<StateIdEnum> = None;
    if let Some(current_id_clone) = self.current_state_id.clone() {
      if let Some(current_s_impl) = self.states.get_mut(&current_id_clone) {
        next_state_id_opt = current_s_impl.on_update(context);
      } else {
        eprintln!(
          "FSM Warning: Current state ID {:?} not found in states map during update.",
          current_id_clone
        );
      }
    }
    if let Some(next_id) = next_state_id_opt {
      self.transition(context, next_id);
    }
  }

  pub fn transition(&mut self, context: &mut dyn FsmContext<Op, AppID>, next_state_id: StateIdEnum) {
    if !self.states.contains_key(&next_state_id) {
      panic!("Attempted to transition to un-added state ID {:?}", next_state_id);
    }
    let previous_id_opt = self.current_state_id.clone();
    if let Some(ref current_id_val) = previous_id_opt {
      if let Some(current_s_impl) = self.states.get_mut(current_id_val) {
        current_s_impl.on_exit(context, &next_state_id);
      }
    }
    self.current_state_id = Some(next_state_id.clone());
    let next_s_impl = self
      .states
      .get_mut(&next_state_id)
      .expect("FSM logic error: State checked for existence but not found for on_enter.");
    next_s_impl.on_enter(context, previous_id_opt);
  }
}
