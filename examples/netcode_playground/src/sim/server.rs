//! The authoritative server, simulated in-process.
//!
//! This is a plain synchronous struct, not a plaza `StateController`: core is
//! async and does not target wasm, and a Gambetta-style demo wants the server in
//! the same loop anyway. It owns every box, moves the bots, applies the local
//! player's inputs in order, and clamps everyone to the arena.

use plaza_client_utils::FixedTimestep;
use plaza_client_utils::types::SequenceNumber;
use plaza_client_utils::RttEstimator;
use plaza_server_utils::HistoricalStateBuffer;

use crate::sim::types::{apply_input, clamp_to_arena, BoxState, ClientCmd, EntityId, ServerPacket, Shot, ShotResult, Vec2, ARENA_H, ARENA_W, HIT_RADIUS, YOU};

/// Enough rewind history for any latency the sliders allow (round trip plus
/// interpolation delay) even at the highest server rate.
const HISTORY_LEN: usize = 128;

pub struct ToyServer {
  you: BoxState,
  last_applied_seq: SequenceNumber,
  pending: Vec<ClientCmd>,
  bots: Vec<Bot>,

  clock_ms: u64,
  /// The server tick. Its rate is a live setting, so the step is set per advance
  /// rather than fixed at construction.
  step: FixedTimestep,

  /// Past bot positions, so a shot can be judged against where they *were* when
  /// the shooter saw them. This is the lag-compensation building block.
  history: HistoricalStateBuffer<EntityId, BoxState, u64>,

  /// The server's measured round trip to the player, its "latency to player".
  rtt: RttEstimator,
}

struct Bot {
  id: EntityId,
  center: Vec2,
  radius: f32,
  omega: f32,
  phase: f32,
}

impl ToyServer {
  pub fn new(bot_count: u8) -> Self {
    let bots = (0..bot_count)
      .map(|i| {
        let f = i as f32;
        Bot {
          id: i + 1,
          center: Vec2::new(ARENA_W * (0.3 + 0.4 * (f / bot_count.max(1) as f32)), ARENA_H * 0.5),
          radius: 90.0 + 40.0 * f,
          omega: 1.1 - 0.25 * f,
          phase: f * 1.7,
        }
      })
      .collect();

    Self {
      you: BoxState {
        pos: Vec2::new(ARENA_W * 0.5, ARENA_H * 0.5),
        vel: Vec2::default(),
      },
      last_applied_seq: 0,
      pending: Vec::new(),
      bots,
      clock_ms: 0,
      step: FixedTimestep::from_step_ms(1),
      history: HistoricalStateBuffer::new(HISTORY_LEN),
      rtt: RttEstimator::default(),
    }
  }

  /// Records a measured round trip to the player (from a returned ping).
  pub fn observe_rtt(&mut self, sample_ms: u64) {
    self.rtt.observe(sample_ms);
  }

  /// The server's smoothed round trip to the player, if measured yet.
  pub fn rtt_ms(&self) -> Option<f32> {
    self.rtt.rtt()
  }

  /// Accepts a delivered client command. Applied on the next server tick.
  ///
  /// Bounded, because any queue fed by a peer and drained by a local clock has
  /// to be: a client that floods commands between two ticks (or a server that
  /// stops ticking) must cost itself its own oldest inputs, not this process's
  /// memory. Oldest dropped rather than newest, because with discrete inputs
  /// the newest intent is the one that still matters.
  pub fn receive(&mut self, cmd: ClientCmd) {
    const PENDING_CAP: usize = 256;
    if self.pending.len() >= PENDING_CAP {
      self.pending.sort_by_key(|c| c.seq);
      self.pending.remove(0);
    }
    self.pending.push(cmd);
  }

  /// Advances by `dt_ms`, emitting a packet for each server tick of `step_ms`
  /// crossed (usually zero or one per render frame, since the server ticks
  /// slower). `step_ms` can change frame to frame: the server rate is dynamic.
  pub fn advance(&mut self, dt_ms: u64, step_ms: u64) -> Vec<ServerPacket> {
    self.step.set_step_ms(step_ms.max(1));
    let mut packets = Vec::new();
    for step in self.step.advance(dt_ms) {
      self.clock_ms += step.as_millis() as u64;
      packets.push(self.tick());
    }
    packets
  }

  fn tick(&mut self) -> ServerPacket {
    // Apply this player's inputs in sequence order, skipping any already seen.
    self.pending.sort_by_key(|c| c.seq);
    for cmd in self.pending.drain(..).collect::<Vec<_>>() {
      if cmd.seq <= self.last_applied_seq {
        continue;
      }
      apply_input(&mut self.you, &cmd.input, &());
      clamp_to_arena(&mut self.you);
      self.last_applied_seq = cmd.seq;
    }

    let remotes: Vec<(EntityId, BoxState)> = self
      .bots
      .iter()
      .map(|b| (b.id, b.state_at(self.clock_ms)))
      .collect();

    // Record where each bot is this tick, so a later shot can be rewound to it.
    // A real server has no analytic oracle for the past; it remembers, exactly
    // as this buffer does.
    for (id, state) in &remotes {
      self.history.record_state(*id, self.clock_ms, *state);
    }

    ServerPacket {
      server_time_ms: self.clock_ms,
      you: (self.you, self.last_applied_seq),
      remotes,
    }
  }

  /// Judges a shot. With `lag_comp`, each bot is rewound to the time the shooter
  /// was seeing (`shot.aim_time`) before the hit test; without, it is judged
  /// against the present, so a shot at a moving target under latency misses.
  pub fn resolve_shot(&self, shot: Shot, lag_comp: bool) -> ShotResult {
    for bot in &self.bots {
      let state = if lag_comp {
        self.history.get_state_at_or_before(&bot.id, shot.aim_time)
      } else {
        Some(bot.state_at(self.clock_ms))
      };
      if let Some(state) = state
        && state.pos.dist(shot.aim) <= HIT_RADIUS
      {
        return ShotResult {
          aim: shot.aim,
          hit: Some(bot.id),
        };
      }
    }
    ShotResult { aim: shot.aim, hit: None }
  }

  /// Authoritative positions right now, for the "truth" overlay the demo draws
  /// underneath the interpolated remotes.
  pub fn truth(&self) -> Vec<(EntityId, BoxState)> {
    let mut out = vec![(YOU, self.you)];
    out.extend(self.bots.iter().map(|b| (b.id, b.state_at(self.clock_ms))));
    out
  }
}

impl Bot {
  fn state_at(&self, time_ms: u64) -> BoxState {
    let t = time_ms as f32 / 1000.0;
    let a = self.omega * t + self.phase;
    let pos = Vec2::new(self.center.x + self.radius * a.cos(), self.center.y + self.radius * a.sin());
    let vel = Vec2::new(-self.radius * self.omega * a.sin(), self.radius * self.omega * a.cos());
    BoxState { pos, vel }
  }
}
