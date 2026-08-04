//! Server and clients in one process, with an impaired link between them.
//!
//! It also does the accounting the panel shows, because the split between the
//! derived half of the traffic and the streamed half is only visible where
//! every outbound op passes through one place.

use plaza_client_utils::net_sim::{LatencyLink, Rng};

use crate::sim::client::Client;
use crate::sim::protocol::{Intent, Op, wire_cost};
use crate::sim::server::Server;
use crate::sim::types::{Controls, Dir8, PlayerId, SIM_STEP_MS};

const IMPAIR_SEED: u64 = 0xC0_FF_EE_11;

pub struct World {
  pub server: Server,
  pub clients: Vec<Client>,
  down: Vec<LatencyLink<Op>>,
  up: LatencyLink<(usize, Op)>,
  rng: Rng,
}

impl World {
  pub fn new(controls: &Controls, seed: u64) -> Self {
    let count = controls.players.clamp(1, crate::sim::types::MAX_SEATS);
    let mut server = Server::new(count, seed);
    let mut clients = Vec::new();
    for seat in 0..count {
      server.take_seat(seat);
      let mut client = Client::new(seat as PlayerId, controls.render_delay_ms);
      client.on_op(
        Op::Welcome {
          player: seat as PlayerId,
          policy: server.policy(controls),
          start: Box::new(server.start()),
        },
        0,
      );
      clients.push(client);
    }
    Self {
      server,
      clients,
      down: (0..count).map(|_| LatencyLink::default()).collect(),
      up: LatencyLink::default(),
      rng: Rng::new(IMPAIR_SEED),
    }
  }

  pub fn now_ms(&self) -> u64 {
    self.server.now_ms()
  }

  fn send_up(&mut self, seat: usize, op: Op, controls: &Controls) {
    let now = self.server.now_ms();
    self.up.send(now, (seat, op), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut self.rng);
  }

  pub fn fly(&mut self, seat: usize, dir: Dir8, controls: &Controls) {
    let now = self.server.now_ms();
    let Some(client) = self.clients.get_mut(seat) else { return };
    if let Some(op) = client.press(dir, now) {
      self.send_up(seat, op, controls);
    }
  }

  pub fn fire(&mut self, seat: usize, controls: &Controls) {
    let now = self.server.now_ms();
    let Some(client) = self.clients.get_mut(seat) else { return };
    if let Some(op) = client.fire(now) {
      self.send_up(seat, op, controls);
    }
  }

  pub fn step(&mut self, dt_ms: u64, controls: &Controls) {
    let now = self.server.now_ms();
    for (seat, op) in self.up.drain_due(now) {
      match op {
        Op::Move { tick, dir, .. } => {
          self.server.submit(seat, tick, Intent::Move(dir), controls);
        }
        Op::Fire { tick, .. } => {
          self.server.submit(seat, tick, Intent::Fire, controls);
        }
        Op::Struck { tick, .. } => {
          self.server.submit(seat, tick, Intent::Struck, controls);
        }
        _ => {}
      }
    }

    let out = self.server.advance(dt_ms, controls);
    let now = self.server.now_ms();

    let mut outbound: Vec<Op> = Vec::new();
    // Causes first. A wave has to reach a client before the frame that shows
    // its bullets already on screen, or the curtain appears out of nothing.
    for wave in out.waves {
      outbound.push(Op::WaveUp(Box::new(wave)));
    }
    for down in out.downed {
      outbound.push(Op::ArmDown(down));
    }
    for death in out.deaths {
      outbound.push(Op::Died(Box::new(death)));
    }
    for frame in out.frames {
      outbound.push(Op::Frame(Box::new(frame)));
    }

    self.account(&outbound);

    for link in &mut self.down {
      for op in &outbound {
        link.send(now, op.clone(), controls.latency_ms, controls.jitter_ms, controls.loss_pct, &mut self.rng);
      }
    }

    for seat in 0..self.clients.len() {
      for op in self.down[seat].drain_due(now) {
        self.clients[seat].on_op(op, now);
      }
      if let Some(declaration) = self.clients[seat].advance(dt_ms, now, controls) {
        self.send_up(seat, declaration, controls);
      }
    }
  }

  /// Prices one tick of outbound traffic, split by what produced it.
  fn account(&mut self, outbound: &[Op]) {
    if outbound.is_empty() {
      return;
    }
    let derivable: Vec<Op> = outbound.iter().filter(|op| wire_cost::is_derivable_half(op)).cloned().collect();
    let streamed: Vec<Op> = outbound.iter().filter(|op| !wire_cost::is_derivable_half(op)).cloned().collect();
    let stats = &mut self.server.stats;
    stats.bytes_derivable += wire_cost::bytes(&derivable) as u64;
    stats.bytes_streamed += wire_cost::bytes(&streamed) as u64;
    stats.bytes_total += wire_cost::bytes(outbound) as u64;
    stats.bytes_numerically_tagged += wire_cost::bytes_numerically_tagged(outbound) as u64;
  }

  pub fn run(&mut self, ms: u64, controls: &Controls) {
    for _ in 0..(ms / SIM_STEP_MS) {
      self.step(SIM_STEP_MS, controls);
    }
  }

  /// Bytes per enemy bullet on screen, against bytes per player bullet.
  ///
  /// The comparison the example is built to make. The first number falls
  /// towards nothing as the curtain thickens; the second does not move.
  pub fn cost_per_bullet(&self) -> (f32, f32) {
    let curtain = self.server.curtain().len().max(1) as f32;
    let streamed = self.server.bullets.len().max(1) as f32;
    (
      self.server.stats.bytes_derivable as f32 / curtain,
      self.server.stats.bytes_streamed as f32 / streamed,
    )
  }

  pub fn total_snaps(&self) -> u64 {
    self.clients.iter().map(|c| c.stats.snaps).sum()
  }

  /// Whether every client derived exactly the field the server did.
  ///
  /// The property the whole design rests on, and the only one nothing on the
  /// wire would ever reveal: if two machines disagree about a curtain neither
  /// of them describes, nobody finds out.
  /// Compared per wave, not in total.
  ///
  /// A wave announcement takes a one-way trip like anything else, so a client
  /// legitimately has fewer waves than the server for a moment after each one
  /// starts. The claim is not that the two fields are always identical, it is
  /// that **for every wave a client knows about, its bullets are exactly the
  /// server's**: agreement is per cause, and the causes arrive when they
  /// arrive.
  pub fn curtains_agree(&self) -> bool {
    let truth = self.server.curtain();
    self.clients.iter().all(|client| {
      if client.sim_tick() != self.server.tick() {
        return true;
      }
      client.waves.iter().all(|wave| {
        // A death that has not landed yet is the same story one level down, so
        // a wave with a pending `ArmDown` is skipped rather than failed.
        if self.server.downed.iter().any(|d| d.wave == wave.id) && !client.downed.iter().any(|d| d.wave == wave.id) {
          return true;
        }
        let mine: Vec<_> = client.curtain().iter().filter(|b| b.wave == wave.id).collect();
        let theirs: Vec<_> = truth.iter().filter(|b| b.wave == wave.id).collect();
        mine.len() == theirs.len()
          && mine
            .iter()
            .zip(theirs.iter())
            .all(|(a, b)| a.arm == b.arm && a.index == b.index && a.pos.dist(b.pos) < 0.01)
      })
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::sim::types::{DeathRule, V2};

  const SEED: u64 = 0x7A_11_3D_05;

  fn base() -> Controls {
    Controls {
      bots: true,
      players: 2,
      latency_ms: 0,
      jitter_ms: 0,
      loss_pct: 0.0,
      ..Controls::default()
    }
  }

  #[test]
  fn every_client_derives_exactly_the_curtain_the_server_has() {
    // The property the whole design rests on, and the only one nothing on the
    // wire would ever reveal: two machines that disagree about a field neither
    // of them describes disagree in silence.
    let controls = Controls { latency_ms: 120, jitter_ms: 30, ..base() };
    let mut world = World::new(&controls, SEED);
    for _ in 0..500 {
      world.step(SIM_STEP_MS, &controls);
      assert!(world.curtains_agree(), "a client's curtain drifted at tick {}", world.server.tick());
    }
    assert!(world.server.curtain().len() > 50, "and there was a curtain to agree about");
  }

  #[test]
  fn latency_never_changes_the_curtain_by_one_bullet() {
    // Because it is a function of the tick, and a tick is not a wall clock.
    // The comparison seed_defense could not make, because it had no half that
    // *was* affected to compare against.
    let quick = Controls { latency_ms: 0, jitter_ms: 0, ..base() };
    let slow = Controls { latency_ms: 300, jitter_ms: 60, ..base() };

    let mut a = World::new(&quick, SEED);
    let mut b = World::new(&slow, SEED);
    a.run(6000, &quick);
    b.run(6000, &slow);

    assert_eq!(
      a.server.curtain().len(),
      b.server.curtain().len(),
      "the same tick produced two different curtains on two different links"
    );
  }

  #[test]
  fn the_derivable_half_gets_cheaper_per_bullet_and_the_streamed_half_does_not() {
    // The headline comparison. Both halves are on the same wire in the same
    // game, so this is a like-for-like measurement rather than two examples
    // quoted at each other.
    let controls = base();
    let mut world = World::new(&controls, SEED);
    world.run(10_000, &controls);

    let (derived, streamed) = world.cost_per_bullet();
    assert!(world.server.curtain().len() > 50, "there is a curtain");
    assert!(
      derived < streamed,
      "derived {derived:.2} bytes per enemy bullet against {streamed:.2} per player bullet"
    );
  }

  #[test]
  fn the_share_of_the_wire_that_is_variant_names_is_worth_knowing() {
    // The number `IMPROVEMENTS` gates float quantization, bit packing and
    // numeric variant tags on. Taken over real traffic rather than a synthetic
    // message, because the share depends entirely on the mix.
    let controls = base();
    let mut world = World::new(&controls, SEED);
    world.run(8000, &controls);
    let share = world.server.stats.variant_name_share();
    assert!(world.server.stats.bytes_total > 1000, "there was traffic to measure");
    assert!(share > 0.0, "compact msgpack is positional for fields and not for variants");
    assert!(share < 0.5, "implausible share {share}");
  }

  #[test]
  fn the_client_always_knows_first_and_the_rule_only_decides_if_it_may_act() {
    // Not the claim this example was planned around, and a better one. A
    // derivable curtain means the client computed the same field the server
    // did and saw the contact on the same tick, so nobody is ever the last to
    // find out. What `ServerOnly` costs is not knowledge, it is permission:
    // the player watches themself keep flying for a round trip after they
    // already know they are dead, which is worse than not knowing.
    let slow = Controls {
      latency_ms: 200,
      playout_delay_ms: 250,
      ..base()
    };

    let mut told = World::new(&Controls { death_rule: DeathRule::ServerOnly, ..slow }, SEED);
    told.run(20_000, &Controls { death_rule: DeathRule::ServerOnly, ..slow });

    let acting = Controls { death_rule: DeathRule::ServerConfirms, ..slow };
    let mut acts = World::new(&acting, SEED);
    acts.run(20_000, &acting);

    assert!(told.server.stats.deaths > 0, "somebody has to die for this to mean anything");
    assert_eq!(told.server.stats.declared, 0, "the server-only rule never asks");
    assert!(acts.server.stats.declared > 0, "the confirming rule does");

    let waited: u64 = told.clients.iter().map(|c| c.stats.flown_while_dead_ticks).sum();
    let acted: u64 = acts.clients.iter().map(|c| c.stats.flown_while_dead_ticks).sum();
    assert!(waited > 0, "nobody spent a tick flying a ship they knew was hit");
    assert!(acted < waited, "acting on your own contact cost {acted} ticks against {waited} waiting");

    // Both clients saw the contact. That is the part worth pinning: the
    // difference is never who knew.
    let seen_told: u64 = told.clients.iter().map(|c| c.stats.contacts_seen).sum();
    assert!(seen_told > 0, "the server-only client saw its own contacts too");
  }

  #[test]
  fn a_ship_that_stops_declaring_is_immortal_and_obvious() {
    // Both halves of the answer to "how cheatable is letting the ship decide".
    // Completely, and completely visible, because the server derives the same
    // curtain and can count the contacts nobody owned up to for free.
    let controls = Controls {
      death_rule: DeathRule::ClientDeclares,
      silent_seat: true,
      ..base()
    };
    let mut world = World::new(&controls, SEED);
    world.run(20_000, &controls);

    assert_eq!(world.clients[0].stats.declared, 0, "seat zero said nothing");
    assert!(world.clients[0].stats.contacts_seen > 0, "and it was hit repeatedly");
    assert!(
      world.server.stats.undeclared > 0,
      "the server saw {} contacts and counted none of them as unowned",
      world.server.stats.server_found
    );
  }

  #[test]
  fn an_honest_ship_is_not_accused_of_going_quiet() {
    // The other half, and the one whose absence would be silent: a detector
    // that fires on honest play is not a detector, it is a false-positive
    // generator with a plausible name.
    let controls = Controls {
      death_rule: DeathRule::ClientDeclares,
      silent_seat: false,
      ..base()
    };
    let mut world = World::new(&controls, SEED);
    world.run(20_000, &controls);

    let silent = Controls { silent_seat: true, ..controls };
    let mut cheat = World::new(&silent, SEED);
    cheat.run(20_000, &silent);

    assert!(
      cheat.server.stats.undeclared > world.server.stats.undeclared,
      "honest {} against silent {}",
      world.server.stats.undeclared,
      cheat.server.stats.undeclared
    );
  }

  #[test]
  fn a_declaration_the_curtain_disagrees_with_is_refused() {
    // The rule that is both fair and checkable, and it is only checkable
    // because the curtain is a function of the tick: the server recomputes the
    // exact field the client dodged, at the tick that was named.
    let controls = Controls {
      death_rule: DeathRule::ServerConfirms,
      ..base()
    };
    let mut world = World::new(&controls, SEED);
    world.run(4000, &controls);

    // A claim about a tick with nothing anywhere near this ship.
    let far_past = world.server.tick().saturating_sub(5);
    let before = world.server.stats.declared_refused;
    world.server.ships[0].pos = V2::new(-9000.0, -9000.0);
    world.server.submit(0, far_past, Intent::Struck, &controls);
    world.server.ships[0].pos = crate::sim::types::Ship::spawn(0).pos;
    assert!(
      world.server.stats.declared_refused > before,
      "a claim about a tick this ship spent nowhere near a bullet was believed"
    );
  }

  #[test]
  fn latency_alone_produces_no_disagreement_about_where_a_ship_is() {
    let controls = Controls {
      latency_ms: 120,
      jitter_ms: 30,
      playout_delay_ms: 200,
      bots: false,
      ..base()
    };
    let mut world = World::new(&controls, SEED);
    const DIRS: [Dir8; 4] = [Dir8::E, Dir8::N, Dir8::W, Dir8::S];
    let mut t = 0;
    while t < 8000 {
      for seat in 0..world.clients.len() {
        world.fly(seat, DIRS[((t / 400) as usize + seat) % DIRS.len()], &controls);
      }
      world.run(200, &controls);
      t += 200;
    }
    assert_eq!(world.total_snaps(), 0, "a delayed ship was corrected");
  }

  #[test]
  fn a_joiner_gets_the_waves_already_in_flight() {
    // The one failure mode a derived field has that a streamed one does not:
    // a client told only about future waves flies through a curtain it cannot
    // see, and nothing about the frames it is receiving would say so.
    let controls = base();
    let mut world = World::new(&controls, SEED);
    world.run(4000, &controls);
    assert!(!world.server.waves.is_empty(), "there are waves up");

    let start = world.server.start();
    assert_eq!(start.waves.len(), world.server.waves.len());
    let mut late = crate::sim::client::Client::new(1, controls.render_delay_ms);
    late.on_op(
      Op::Welcome {
        player: 1,
        policy: world.server.policy(&controls),
        start: Box::new(start),
      },
      0,
    );
    late.advance(SIM_STEP_MS, world.server.now_ms(), &controls);
    assert_eq!(
      late.curtain().len(),
      world.server.curtain().len(),
      "a joiner's first frame had a different curtain from everybody else's"
    );
  }
}
