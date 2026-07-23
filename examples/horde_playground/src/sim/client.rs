//! One player's client: holds only the entities relevant to it, draws them by
//! one of three strategies, and counts how often a packet refers to an entity it
//! no longer holds.
//!
//! That last part is the experiment about generational handles. A handle names a
//! slot *and* its occupant; if the generation is discarded, a reference to a dead
//! entity silently lands on whatever now occupies its slot. Whether that actually
//! happens here is measured, not assumed.

use std::collections::BTreeMap;

use plaza_client_utils::{ease_in_quad, ErrorSmoother};
use plaza_client_utils::ack::AckWindow;
use plaza_server_utils::relevance::SetDigest;

use crate::sim::types::{coin_pull, repulsor_pulse, step_coin, Coin, CoinId, Crowd, Upgrade, Wallet, COIN_FLIGHT_MS, COIN_PICKUP_RADIUS, step_enemy, Controls, Enemy, EnemyKind, Handle, LeaveReason, Packet, PlayerId, Projectile, RemoteMode, Vec2, SIM_DT};

const SMOOTH_SECS: f32 = 0.25;

fn lerp(a: &Vec2, b: &Vec2, t: f32) -> Vec2 {
  Vec2::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

struct RemoteEnemy {
  /// The generation this client believes it holds, for detecting stale refs.
  generation: u16,
  /// Kind came in the spawn and never changes, so it is never re-sent.
  kind: EnemyKind,
  sim: Enemy,
  smoother: ErrorSmoother<Vec2>,
  last_pos: Vec2,
  last_ms: u64,
  prev_pos: Vec2,
  prev_ms: u64,
}

/// How long an announcement stays on screen.
const NOTICE_SECS: f32 = 3.0;

/// A coin travelling to whoever won it.
#[derive(Clone, Copy, Debug)]
pub struct CoinFlight {
  pub id: CoinId,
  /// Where it was when the claim landed. Kept so the curve has an origin; a
  /// formulation without one can only ever arrive linearly.
  from: Vec2,
  pub to_player: PlayerId,
  elapsed_ms: f32,
}

impl CoinFlight {
  /// Where to draw it, given where its owner is *now*.
  ///
  /// Re-aimed at the live player position every frame rather than at a remembered
  /// one, because the owner is moving: a path computed once at claim time would
  /// land where they used to be.
  pub fn at(&self, owner_now: Vec2) -> Vec2 {
    // Quadratic, not cubic. Both accelerate into the player; cubic covers 12.5%
    // of the distance by half time against quadratic's 25%, and over a flight
    // this short that difference is the whole readability of the effect.
    let t = ease_in_quad((self.elapsed_ms / COIN_FLIGHT_MS).clamp(0.0, 1.0));
    Vec2::new(self.from.x + (owner_now.x - self.from.x) * t, self.from.y + (owner_now.y - self.from.y) * t)
  }

  pub fn done(&self) -> bool {
    self.elapsed_ms >= COIN_FLIGHT_MS
  }
}

pub struct Client {
  pub id: PlayerId,
  enemies: BTreeMap<Handle, RemoteEnemy>,
  players: Vec<Vec2>,
  pub projectiles: Vec<Projectile>,
  now_ms: u64,

  /// A reference whose generation did not match what we hold. With generations
  /// on these are rejected; with them off the same reference would have been
  /// applied to the wrong entity.
  pub stale_refs: u64,
  pub deaths_seen: u64,
  /// Packets after which this client's mirror did not match the server's digest.
  /// A real client would ask for a full resync; here it is counted, because the
  /// point is that the divergence is *visible at all*.
  pub digest_mismatches: u64,
  /// Which packets have arrived. Twelve bytes back up the wire, and the whole
  /// input to the server's recovery: it needs to know which of its deltas the
  /// client is actually holding, and nothing else.
  pub acks: AckWindow,
  /// The distant crowds this client was told about, in place of the entities
  /// themselves. What lets it draw a whole-arena map from its own knowledge
  /// rather than from the server's.
  pub crowds: Vec<Crowd>,
  /// Coins this client can see.
  pub coins: Vec<Coin>,
  /// Coins in flight to the player who won them.
  ///
  /// **Presentation only, and deliberately client-side.** Magnet drift has to be
  /// a shared rule because it changes which player ends up nearest, and therefore
  /// decides the authoritative outcome. The flight happens *after* the claim is
  /// settled, so nothing about it can change the result: simulating it on the
  /// server would spend bandwidth and coupling on an animation, and would create
  /// a third state between "on the field" and "banked" that the loss-recovery
  /// machinery would then have to reason about. As pure presentation, a lost
  /// packet costs an animation rather than a currency inconsistency.
  pub flights: Vec<CoinFlight>,
  /// Every player's wallet as last confirmed.
  pub wallets: Vec<Wallet>,
  /// This client's *believed* balance, which equals the authoritative one unless
  /// balance prediction is on.
  ///
  /// Kept separate rather than overwriting `wallets[id].balance`, so the two can
  /// be compared. A single field would make the error unmeasurable, which is the
  /// mistake that has hidden four separate defects in this project already.
  pub believed_balance: u32,
  /// Coins this client predicted it won. Cleared when the server confirms or
  /// denies.
  predicted_claims: Vec<CoinId>,
  /// Predictions the server gave to somebody else. The cost of predicting a
  /// discrete event: unlike a position, this correction cannot be eased.
  pub denied_claims: u64,
  /// Upgrades this client believes it owns, which may run ahead of the wallet
  /// while a purchase is in flight.
  pub believed_upgrades: Vec<Upgrade>,
  /// Packets received while this client believed it owned upgrades the server
  /// disagreed about.
  ///
  /// Cumulative, because the interesting quantity is the *window* during which
  /// the client was simulating under the wrong rule, and an end-of-run snapshot
  /// cannot see a transient that has since resolved. Sampling a settled state and
  /// reporting zero is the same mistake as checking `in_sync` mid-recovery.
  pub wrong_rule_packets: u64,
  /// Recent things worth telling the player about, newest last, with the age of
  /// each in seconds.
  ///
  /// Client-side and derived from what arrives, not sent by the server. An
  /// announcement is presentation: the server already communicated the fact by
  /// changing the wallet, and spending wire bytes to also say it in words would
  /// be paying twice for one event.
  pub notices: Vec<(String, f32)>,
  /// Estimated server clock, advanced locally between packets.
  est_server_ms: u64,
  /// Purchases asked for and not yet answered.
  ///
  /// Tracked so a request is made once rather than every frame until it lands.
  /// Without this the client spams the same request for a whole round trip, and
  /// the refusal count measures its own impatience instead of any real
  /// disagreement about the balance.
  pending_buys: Vec<Upgrade>,
  /// The newest sequence this client has actually *applied*. Only these are
  /// acknowledged, because that is what the server needs to know: not what
  /// arrived, but which state the client is in.
  applied_seq: Option<u64>,

}

impl Client {
  pub fn new(id: PlayerId, player_count: usize) -> Self {
    Self {
      id,
      enemies: BTreeMap::new(),
      players: vec![Vec2::default(); player_count],
      projectiles: Vec::new(),
      now_ms: 0,
      stale_refs: 0,
      deaths_seen: 0,
      digest_mismatches: 0,
      acks: AckWindow::new(),
      crowds: Vec::new(),
      coins: Vec::new(),
      flights: Vec::new(),
      wallets: vec![Wallet::default(); player_count],
      believed_balance: 0,
      predicted_claims: Vec::new(),
      denied_claims: 0,
      believed_upgrades: Vec::new(),
      notices: Vec::new(),
      est_server_ms: 0,
      pending_buys: Vec::new(),
      wrong_rule_packets: 0,
      applied_seq: None,

    }
  }

  /// Which players this client believes are repelling enemies.
  ///
  /// Its own entry comes from `believed_upgrades`, which under prediction can run
  /// ahead of the truth. That is deliberate: it is the path by which a wrong
  /// purchase becomes a wrong simulation rather than only a wrong number.
  fn repel_flags(&self) -> Vec<Option<f32>> {
    // Evaluated against this client's *estimate* of server time, which is what
    // makes the pulse a consumer of clock sync as well as of the upgrade flag: a
    // client whose clock is off fires at the wrong moment and its enemies scatter
    // when the server's do not.
    let pulse = repulsor_pulse(self.est_server_ms);
    (0..self.players.len())
      .map(|p| {
        let owns = if p == self.id as usize {
          self.believed_upgrades.contains(&Upgrade::Repulsor)
        } else {
          self.wallets.get(p).is_some_and(|w| w.has(Upgrade::Repulsor))
        };
        if owns { pulse } else { None }
      })
      .collect()
  }

  /// Coins in flight, with where to draw each one this frame.
  pub fn flight_positions(&self) -> Vec<Vec2> {
    self
      .flights
      .iter()
      .map(|f| f.at(self.players[f.to_player as usize % self.players.len()]))
      .collect()
  }

  /// The active pulse radius for one player, as this client believes it.
  pub fn repel_radius(&self, player: usize) -> Option<f32> {
    self.repel_flags().get(player).copied().flatten()
  }

  /// This client's estimate of the server clock, which the pulse phase is read
  /// from.
  pub fn est_server_ms(&self) -> u64 {
    self.est_server_ms
  }

  /// Guesses which coins this client just won, and takes the credit immediately.
  ///
  /// The rule is the server's own: nearest player inside the radius claims it. The
  /// catch is the inputs. Remote player positions are a latency out of date, so
  /// when two players converge on one coin both can conclude they were nearest,
  /// and one of them is about to be told otherwise. That is the whole point of
  /// the toggle: the prediction is *usually* right, and being wrong costs a snap
  /// that cannot be smoothed away.
  fn predict_claims(&mut self) {
    let me = self.players[self.id as usize];
    let others: Vec<Vec2> = self.players.iter().enumerate().filter(|(p, _)| *p != self.id as usize).map(|(_, v)| *v).collect();
    let id = self.id;
    let mut won: Vec<(CoinId, Vec2)> = Vec::new();
    self.coins.retain(|coin| {
      let mine = coin.pos.dist(me);
      if mine > COIN_PICKUP_RADIUS {
        return true;
      }
      if others.iter().any(|o| coin.pos.dist(*o) < mine) {
        return true;
      }
      let _ = id;
      won.push((coin.id, coin.pos));
      false
    });
    for (coin, from) in won {
      self.predicted_claims.push(coin);
      self.flights.push(CoinFlight {
        id: coin,
        from,
        to_player: self.id,
        elapsed_ms: 0.0,
      });
    }
    self.believed_balance = self.wallets[self.id as usize].balance + self.predicted_claims.len() as u32;
  }

  /// The cheapest upgrade this client believes it can afford and does not own.
  ///
  /// Judged against `believed_balance`, which is the point: with prediction on
  /// that number can be running ahead of the truth, so the client will ask for
  /// something it cannot actually pay for and the server will refuse.
  pub fn wants_to_buy(&mut self) -> Option<Upgrade> {
    let choice = Upgrade::ALL
      .iter()
      .filter(|u| !self.believed_upgrades.contains(u) && !self.pending_buys.contains(u) && self.believed_balance >= u.cost())
      .min_by_key(|u| u.cost())
      .copied();
    // Records the request and nothing else. What the client *believes* it owns is
    // derived in one place, on packet receipt, from the confirmed wallet plus
    // whatever is pending. Optimistically claiming it here as well made the
    // belief run ahead even with prediction switched off, which showed up as a
    // wrong-rule window in the control configuration.
    if let Some(u) = choice {
      self.pending_buys.push(u);
    }
    choice
  }

  pub fn known_entities(&self) -> usize {
    self.enemies.len()
  }

  /// Every player position as last known, for drawing peers.
  pub fn players(&self) -> &[Vec2] {
    &self.players
  }

  /// Overrides the local player's position with a locally predicted one.
  ///
  /// The networked client predicts its own movement (the server confirms it a
  /// round trip later), and the coin and repulsor rules this client runs read the
  /// local player position, so they should read the predicted one rather than the
  /// stale authoritative copy the last packet carried.
  pub fn set_local_pos(&mut self, pos: Vec2) {
    if (self.id as usize) < self.players.len() {
      self.players[self.id as usize] = pos;
    }
  }

  pub fn on_packet(&mut self, packet: &Packet, recv_ms: u64, controls: &Controls) {
    // Every packet is applied, whatever baseline it names.
    //
    // Worth stating because the instinct is the opposite, and the instinct is
    // what a strict delta protocol requires: if you cannot reach the baseline,
    // discard. That is right when deltas are *relative* (add three, rotate by
    // ten). These are not. `entered` carries the entity in full, `left` names it
    // outright, and a sample is an absolute position, so applying them is
    // idempotent and applying a superset is harmless. Discarding instead starves
    // the client, and measurably: an earlier version of this did, and at 25% loss
    // the mirror emptied out while every agreement check read perfect.
    self.applied_seq = Some(packet.seq);
    self.acks.observe(packet.seq);
    self.now_ms = recv_ms;
    self.est_server_ms = packet.server_time_ms;
    let generational = controls.generational_ids;

    for (p, pos) in &packet.players {
      if (*p as usize) < self.players.len() {
        self.players[*p as usize] = *pos;
      }
    }
    self.projectiles = packet.projectiles.clone();
    self.crowds.clone_from(&packet.crowds);
    // Where each coin was *before* this packet overwrites the list, so a claim can
    // launch its flight from where the player last saw it rather than from
    // nowhere. The server has already removed a claimed coin, so by the time the
    // claims are read its position is only recoverable from the old list.
    let previously_owned = self.wallets[self.id as usize].upgrades.clone();
    let was_at: std::collections::BTreeMap<CoinId, Vec2> = self.coins.iter().map(|c| (c.id, c.pos)).collect();
    self.coins.clone_from(&packet.coins);
    self.wallets.clone_from(&packet.wallets);
    // With coins off the server sends no wallets, so this list is empty and the
    // `self.wallets[self.id]` reads below would panic (and did: the first joiner
    // takes seat 3, so the index was 3 into a length of 0). Pad to cover this
    // player with a default wallet, which is exactly the right answer when there
    // is no currency: a zero balance and nothing owned.
    if self.wallets.len() <= self.id as usize {
      self.wallets.resize(self.id as usize + 1, Wallet::default());
    }

    // The authoritative claims. A prediction that is not in this list, for a coin
    // that is no longer on the field, went to somebody else.
    for (winner, coin) in &packet.claims {
      // Launch the flight from wherever it was last seen: the old list, or the
      // current position of a flight this client had already started on a
      // prediction. Re-aiming an in-flight coin rather than restarting it is what
      // keeps a denied prediction from teleporting.
      let from = self
        .flights
        .iter()
        .find(|f| f.id == *coin)
        .map(|f| f.at(self.players[f.to_player as usize % self.players.len()]))
        .or_else(|| was_at.get(coin).copied());
      self.flights.retain(|f| f.id != *coin);
      if let Some(from) = from {
        self.flights.push(CoinFlight {
          id: *coin,
          from,
          to_player: *winner,
          elapsed_ms: 0.0,
        });
      }

      if let Some(pos) = self.predicted_claims.iter().position(|c| c == coin) {
        self.predicted_claims.remove(pos);
        if *winner != self.id {
          // The correction that cannot be eased. A position can be smoothed
          // toward the truth over a few frames; a coin you already showed the
          // player collecting can only be taken back. Dropping the prediction is
          // the whole correction, because the balance is derived from the list.
          self.denied_claims += 1;
        }
      }
    }
    // The believed balance is **the confirmed value plus what is outstanding**,
    // never a number maintained independently.
    //
    // The independent version was the first attempt and it drifted by 115 coins
    // over a run, because it modelled income and not spending: every purchase the
    // server approved decremented the authoritative balance and left the local
    // one untouched. Deriving it instead makes prediction an *offset on confirmed
    // state*, which is the same shape as replaying unacknowledged inputs over an
    // authoritative snapshot, and it cannot drift because there is nothing to
    // drift from. Anything the server does that the client did not model is
    // absorbed for free.
    // A prediction is only outstanding while the coin is still unresolved. If the
    // server's list no longer carries it and no claim named it, it expired, and a
    // prediction with nothing left to confirm it must be dropped or the
    // outstanding count grows without bound.
    let still_there: std::collections::BTreeSet<CoinId> = packet.coins.iter().map(|c| c.id).collect();
    self.predicted_claims.retain(|c| still_there.contains(c));
    // Hide coins already predicted, or the next tick predicts them again. The
    // server keeps sending them because from its point of view nothing has
    // happened yet.
    self.coins.retain(|c| !self.predicted_claims.contains(&c.id));

    // Announce what changed, by diffing the wallet against what we last held
    // rather than from anything the server says explicitly.
    for upgrade in &self.wallets[self.id as usize].upgrades {
      if !previously_owned.contains(upgrade) {
        self.notices.push((format!("{} acquired", upgrade.label()), 0.0));
      }
    }
    for upgrade in &packet.denied_buys {
      self.notices.push((format!("{} refused, not enough coins", upgrade.label()), 0.0));
    }

    // Refusals and confirmations both retire a pending request.
    for upgrade in &packet.denied_buys {
      self.pending_buys.retain(|u| u != upgrade);
    }
    let owned = &self.wallets[self.id as usize].upgrades;
    self.pending_buys.retain(|u| !owned.contains(u));

    let authoritative = self.wallets[self.id as usize].balance;
    let pending_cost: u32 = self.pending_buys.iter().map(|u| u.cost()).sum();
    self.believed_balance = if controls.predict_balance {
      (authoritative + self.predicted_claims.len() as u32).saturating_sub(pending_cost)
    } else {
      authoritative
    };

    // Under prediction the client shows an upgrade the moment it asks for it.
    //
    // This is the coupling worth having in the tree: `believed_upgrades` feeds
    // `step_enemy`, so an optimistic purchase that the server refuses leaves the
    // client simulating enemies under a rule the server is not using, for as long
    // as the refusal takes to arrive. A mispredicted *number* is a cosmetic wrong;
    // a mispredicted *rule* diverges the world.
    self.believed_upgrades.clone_from(owned);
    if controls.predict_balance {
      for u in &self.pending_buys {
        if !self.believed_upgrades.contains(u) {
          self.believed_upgrades.push(*u);
        }
      }
      self.believed_upgrades.sort_unstable();
    }

    // Counted *after* the belief is settled, not before. Checking beforehand
    // compares last packet's belief against this packet's truth, which measures
    // an unavoidable one-packet lag and reports a wrong-rule window even with
    // prediction switched off. What matters is the rule this client is about to
    // simulate under.
    if self.believed_upgrades != self.wallets[self.id as usize].upgrades {
      self.wrong_rule_packets += 1;
    }

    for (handle, reason) in &packet.left {
      if *reason == LeaveReason::Died {
        self.deaths_seen += 1;
      }
      let key = handle.key(generational);
      match self.enemies.get(&key) {
        // Generation mismatch: this names an occupant we no longer hold. With
        // generations off the key would have matched and we would have removed
        // the wrong entity.
        Some(existing) if generational && existing.generation != handle.generation => {
          self.stale_refs += 1;
        }
        Some(_) => {
          self.enemies.remove(&key);
        }
        None => {}
      }
    }

    for spawn in &packet.entered {
      let key = spawn.handle.key(generational);
      self.enemies.insert(
        key,
        RemoteEnemy {
          generation: spawn.handle.generation,
          kind: spawn.kind,
          sim: Enemy {
            pos: spawn.pos,
            target: spawn.target,
            kind: spawn.kind,
            // Health is the server's business; a client only learns of death.
            health: spawn.kind.max_health(),
          },
          smoother: ErrorSmoother::new(if controls.smooth { SMOOTH_SECS } else { 0.0 }),
          last_pos: spawn.pos,
          last_ms: recv_ms,
          prev_pos: spawn.pos,
          prev_ms: recv_ms,
        },
      );
    }

    // How stale this packet is. In-process both sides share a clock, so this is
    // the exact one-way delay; a real client estimates it from clock sync.
    let age_ms = recv_ms.saturating_sub(packet.server_time_ms);
    let players = self.players.clone();
    let repels: Vec<Option<f32>> = self.repel_flags();

    for sample in &packet.samples {
      let key = sample.handle.key(generational);
      let Some(entity) = self.enemies.get_mut(&key) else {
        continue;
      };
      if generational && entity.generation != sample.handle.generation {
        self.stale_refs += 1;
        continue;
      }
      if let Some(t) = sample.target {
        entity.sim.target = t;
      }

      entity.prev_pos = entity.last_pos;
      entity.prev_ms = entity.last_ms;
      entity.last_pos = sample.pos;
      entity.last_ms = packet.server_time_ms;

      if controls.mode == RemoteMode::Simulate {
        // The sample describes the past. Advance it forward by its own age with
        // the shared rule before correcting, so the correction targets *now*.
        let aim = players[entity.sim.target as usize % players.len()];
        let mut projected = Enemy {
          pos: sample.pos,
          target: entity.sim.target,
          kind: entity.kind,
          health: entity.sim.health,
        };
        let steps = ((age_ms as f32 / 1000.0) / SIM_DT) as u32;
        for _ in 0..steps.min(600) {
          step_enemy(&mut projected, aim, repels[entity.sim.target as usize % repels.len()], SIM_DT);
        }
        let seen = entity.smoother.sample(&entity.sim.pos, lerp);
        entity.sim.pos = projected.pos;
        entity.smoother.begin_from(seen);
      }
    }

    // Everything in this packet is applied, so the mirror must now match what the
    // server said it should be. This is the check that a lost or malformed
    // despawn cannot hide from.
    if generational {
      let mine = SetDigest::from_keys(self.enemies.keys().map(|h| ((h.idx as u64) << 16) | h.generation as u64)).digest();
      if mine != packet.visible_digest {
        self.digest_mismatches += 1;
      }
    }
  }

  pub fn tick(&mut self, dt_ms: u64, controls: &Controls) {
    self.now_ms += dt_ms;
    self.est_server_ms += dt_ms;
    let dt = dt_ms as f32 / 1000.0;

    if controls.mode == RemoteMode::Simulate {
      let players = self.players.clone();
      let repels = self.repel_flags();
      for entity in self.enemies.values_mut() {
        let target = entity.sim.target as usize % players.len();
        step_enemy(&mut entity.sim, players[target], repels[target], dt);
      }
    }

    // Coins move under the same shared rule the server runs, so they stay put
    // between packets instead of stuttering, and a magnet looks like a magnet.
    if controls.coins {
      let attractors: Vec<(Vec2, f32, f32)> = (0..self.players.len())
        .map(|p| {
          let magnet = if p == self.id as usize {
            self.believed_upgrades.contains(&Upgrade::Magnet)
          } else {
            self.wallets.get(p).is_some_and(|w| w.has(Upgrade::Magnet))
          };
          let (radius, speed) = coin_pull(magnet);
          (self.players[p], radius, speed)
        })
        .collect();
      for coin in &mut self.coins {
        step_coin(coin, &attractors, dt);
      }
      if controls.predict_balance {
        self.predict_claims();
      }

      // Flights are advanced by frame time and dropped on arrival. Nothing here
      // touches the balance: that moved when the claim was decided, so the coin
      // sprite is only catching up with a number the player already has.
      for flight in &mut self.flights {
        flight.elapsed_ms += dt_ms as f32;
      }
      self.flights.retain(|f| !f.done());
    }

    for notice in &mut self.notices {
      notice.1 += dt;
    }
    self.notices.retain(|(_, age)| *age < NOTICE_SECS);
    for entity in self.enemies.values_mut() {
      entity.smoother.advance(dt);
    }
    // Shots keep flying locally between packets.
    for p in &mut self.projectiles {
      p.pos.x += p.vel.x * dt;
      p.pos.y += p.vel.y * dt;
      p.ttl -= dt;
    }
    self.projectiles.retain(|p| p.ttl > 0.0);
  }

  /// Where this client draws each enemy it knows about.
  pub fn render(&self, controls: &Controls) -> Vec<(Handle, Vec2, EnemyKind)> {
    self
      .enemies
      .iter()
      .map(|(key, e)| {
        let pos = match controls.mode {
          RemoteMode::Simulate => e.smoother.sample(&e.sim.pos, lerp),
          RemoteMode::DeadReckon => {
            let span = e.last_ms.saturating_sub(e.prev_ms) as f32 / 1000.0;
            let ahead = self.now_ms.saturating_sub(e.last_ms) as f32 / 1000.0;
            if span > 1e-3 {
              let vx = (e.last_pos.x - e.prev_pos.x) / span;
              let vy = (e.last_pos.y - e.prev_pos.y) / span;
              Vec2::new(e.last_pos.x + vx * ahead, e.last_pos.y + vy * ahead)
            } else {
              e.last_pos
            }
          }
          RemoteMode::Interpolate => {
            let delay = controls.sync_interval_ms();
            let target = self.now_ms.saturating_sub(delay);
            let span = e.last_ms.saturating_sub(e.prev_ms) as f32;
            if span > 1e-3 {
              let t = (target.saturating_sub(e.prev_ms) as f32 / span).clamp(0.0, 1.0);
              lerp(&e.prev_pos, &e.last_pos, t)
            } else {
              e.last_pos
            }
          }
        };
        (*key, pos, e.kind)
      })
      .collect()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn flight(from: Vec2) -> CoinFlight {
    CoinFlight {
      id: 1,
      from,
      to_player: 0,
      elapsed_ms: 0.0,
    }
  }

  #[test]
  fn a_flight_takes_the_same_time_however_far_it_has_to_go() {
    // The property the whole formulation exists for. A constant-speed move gives
    // a fixed *speed*, so arrival lags with distance and a coin taken from the rim
    // of the pickup radius trails one taken from underfoot. Interpolating over a
    // normalized duration gives a fixed *time* instead.
    let near = flight(Vec2::new(10.0, 0.0));
    let far = flight(Vec2::new(4000.0, 0.0));
    assert_eq!(near.done(), far.done());
    for elapsed in [COIN_FLIGHT_MS * 0.5, COIN_FLIGHT_MS] {
      let (mut a, mut b) = (near, far);
      a.elapsed_ms = elapsed;
      b.elapsed_ms = elapsed;
      assert_eq!(a.done(), b.done(), "both are equally finished at {elapsed}ms");
    }
  }

  #[test]
  fn a_flight_lands_on_the_owner_even_though_the_owner_moved() {
    // Re-aimed every frame rather than following a path computed at claim time,
    // because the target is a player who is still running. A precomputed path
    // would land where they used to be.
    let mut f = flight(Vec2::new(0.0, 0.0));
    f.elapsed_ms = COIN_FLIGHT_MS;
    let moved = Vec2::new(900.0, -400.0);
    let landed = f.at(moved);
    assert!((landed.x - moved.x).abs() < 0.001 && (landed.y - moved.y).abs() < 0.001, "landed at {landed:?}");
  }

  #[test]
  fn the_curve_starts_slow_and_arrives_fast() {
    // Ease-in rather than ease-out: a coin under a pull that grows as it closes,
    // not a correction that should begin at once and settle gently.
    let mut f = flight(Vec2::new(0.0, 0.0));
    let owner = Vec2::new(100.0, 0.0);
    f.elapsed_ms = COIN_FLIGHT_MS * 0.5;
    let halfway = f.at(owner).x;
    assert!(halfway < 50.0, "less than half the distance covered in half the time: {halfway:.1}");
  }
}
