use crate::types::{
  ArenaOp, ArenaState, Runner, FIELD, IDLE_TICKS, IT_SPEED, MOVED, NO_TAG_BACK_TICKS, RUNNER_SPEED,
  TAG_RADIUS,
};
use async_trait::async_trait;
use plaza::{
  agent::Agent,
  session::TargetedOp,
  state_logic::{LogicInput, LogicOutput, SnapshotRequest, StateLogic, StateLogicError},
};
use tracing::{debug, info};

#[derive(Debug, Default)]
pub struct TagLogic;

#[async_trait]
impl StateLogic<ArenaOp, crate::types::PlayerId, ArenaState> for TagLogic {
  async fn process_input(
    &self,
    state: &mut ArenaState,
    input: LogicInput<ArenaOp, crate::types::PlayerId>,
  ) -> Result<LogicOutput<ArenaOp, crate::types::PlayerId>, StateLogicError> {
    match input {
      LogicInput::AgentOps { source, ops } => {
        let Some(id) = source.id_cloned() else {
          return Ok(LogicOutput::none());
        };
        if let Some(runner) = state.runners.get_mut(&id) {
          for op in ops {
            if let ArenaOp::Steer { dx, dy } = op {
              let len = (dx * dx + dy * dy).sqrt();
              if len.is_finite() && len > f32::EPSILON {
                runner.dx = dx / len;
                runner.dy = dy / len;
              }
            }
          }
        }
        Ok(LogicOutput::none())
      }

      LogicInput::AgentJoined { agent } => {
        let Some(id) = agent.id_cloned() else {
          return Ok(LogicOutput::none());
        };
        // Spread joiners around a circle so nobody starts on top of "it".
        let angle = state.runners.len() as f32 * 2.399963;
        let bot = matches!(agent, Agent::Bot(_));
        state.runners.entry(id).or_insert(Runner {
          agent,
          bot,
          x: FIELD / 2.0 + angle.cos() * FIELD / 3.0,
          y: FIELD / 2.0 + angle.sin() * FIELD / 3.0,
          dx: 0.0,
          dy: 0.0,
          tags: 0,
          ticks_as_it: 0,
          idle_ticks: 0,
        });
        // Not made "it" here: a joiner has not moved yet, so the tick's own
        // rule would take the role straight back off them.
        //
        // The one thing a client is told privately, and only once: which
        // runner in the shared world is theirs. Everything after this is the
        // world everyone gets.
        Ok(LogicOutput::ops(vec![TargetedOp::new_system_to(
          id,
          vec![ArenaOp::Welcome { you: id }],
        )]))
      }

      LogicInput::AgentLeft { agent_id } => {
        state.runners.remove(&agent_id);
        if state.it == Some(agent_id) {
          // Left None deliberately: the tick picks whoever is actually moving,
          // which is the same choice it makes for an idle "it".
          state.it = None;
          debug!("it left, role vacant until the next tick");
        }
        if state.prev_it == Some(agent_id) {
          state.prev_it = None;
        }
        Ok(LogicOutput::none())
      }

      LogicInput::TimeStep { delta_time } => {
        state.tick += 1;
        let dt = delta_time.as_secs_f32();

        for (id, runner) in state.runners.iter_mut() {
          let speed = if state.it == Some(*id) { IT_SPEED } else { RUNNER_SPEED };
          let (was_x, was_y) = (runner.x, runner.y);
          runner.x = (runner.x + runner.dx * speed * dt).clamp(0.0, FIELD);
          runner.y = (runner.y + runner.dy * speed * dt).clamp(0.0, FIELD);
          if (runner.x - was_x).abs() > MOVED || (runner.y - was_y).abs() > MOVED {
            runner.idle_ticks = 0;
          } else {
            runner.idle_ticks += 1;
          }
        }

        // Anyone who has stopped is out of play: an idle browser tab that is
        // "it" would otherwise freeze the game for everyone, permanently, and
        // an idle target would pull every chaser into a corner it never leaves.
        // Covers a tab nobody has touched, a runner pinned against a wall, and
        // a client that stopped answering, with one rule and no special cases.
        let it_in_play = state
          .it
          .and_then(|id| state.runners.get(&id))
          .is_some_and(|r| r.idle_ticks < IDLE_TICKS);
        if !it_in_play {
          let next = state
            .runners
            .iter()
            .filter(|(_, r)| r.idle_ticks < IDLE_TICKS)
            .min_by_key(|(id, r)| (r.idle_ticks, **id))
            .map(|(id, _)| *id);
          if next != state.it {
            info!(was = ?state.it, now = ?next, "it went idle, role reassigned");
            state.it = next;
            state.prev_it = None;
            state.no_tag_back_until = 0;
          }
        }

        if let Some(it_id) = state.it {
          if let Some(it) = state.runners.get(&it_id) {
            let (ix, iy) = (it.x, it.y);
            let protected = (state.tick < state.no_tag_back_until).then_some(state.prev_it).flatten();
            let tagged = state
              .runners
              .iter()
              .filter(|(id, r)| {
                **id != it_id && Some(**id) != protected && r.idle_ticks < IDLE_TICKS
              })
              .find(|(_, r)| {
                let (dx, dy) = (r.x - ix, r.y - iy);
                dx * dx + dy * dy <= TAG_RADIUS * TAG_RADIUS
              })
              .map(|(id, _)| *id);

            if let Some(caught) = tagged {
              state.runners.get_mut(&it_id).expect("it exists").tags += 1;
              state.prev_it = Some(it_id);
              state.no_tag_back_until = state.tick + NO_TAG_BACK_TICKS;
              state.it = Some(caught);
              info!(tagger = %it_id, %caught, tick = state.tick, "tag");
            }
          }
          if let Some(it_id) = state.it
            && let Some(it) = state.runners.get_mut(&it_id) {
              it.ticks_as_it += 1;
            }
        }

        if state.runners.is_empty() {
          return Ok(LogicOutput::none());
        }
        // The whole model in one line: every tick, everyone gets the same
        // world. One provider call, one encode, however many runners.
        let everyone = state.runners.values().map(|r| r.agent.clone()).collect();
        Ok(LogicOutput::none().and_snapshot(SnapshotRequest::uniform(everyone)))
      }
    }
  }
}
