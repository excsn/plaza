//! Frame loop: click units into marching and striking, against a live server.
//!
//! Everything an order may do arrives in the view as server-computed options,
//! so the input code here maps clicks onto [`UnitOrders`] and never re-derives
//! a rule. A click the server would refuse is simply not offered.

#[cfg(all(feature = "client", feature = "websocket"))]
mod render;
#[cfg(all(feature = "client", feature = "websocket"))]
mod ui;

use macroquad::prelude::*;
use plaza_example_field_orders::role;
use plaza_example_field_orders::role::Role;

#[cfg(all(feature = "client", feature = "websocket"))]
use plaza_example_field_orders::net::client::{Moment, NetClient, Status};
#[cfg(all(feature = "client", feature = "websocket"))]
use plaza_example_field_orders::protocol::{Army, BattleOp, BattlePhase, Cell};

/// Reports a fatal misconfiguration.
///
/// Never `process::exit` on wasm: there is no process to exit, the call traps,
/// and a browser shows `RuntimeError: unreachable executed` with no reason.
fn give_up(message: String) {
  if cfg!(target_arch = "wasm32") {
    println!("{message}");
  } else {
    eprintln!("{message}");
    std::process::exit(2);
  }
}

/// Reads the role before macroquad opens anything.
///
/// A headless run must never create a window, and macroquad's `#[main]` opens
/// one before the body runs, so the decision happens ahead of it.
fn main() {
  let options = match role::parse(std::env::args()) {
    Ok(options) => options,
    Err(message) => return give_up(message),
  };

  if let Err(message) = role::check_supported(options.role) {
    return give_up(message);
  }
  if options.role == Role::Observer {
    return give_up("field_orders has no observer: join as a client, and you spectate when both seats are taken".to_owned());
  }

  #[cfg(feature = "server")]
  if options.role == Role::Headless {
    let result = tokio::runtime::Runtime::new()
      .expect("tokio runtime")
      .block_on(plaza_example_field_orders::net::host::serve(&options.bind, options.static_dir.clone()));
    if let Err(e) = result {
      eprintln!("server stopped: {e}");
      std::process::exit(1);
    }
    return;
  }

  #[cfg(all(feature = "client", feature = "websocket"))]
  {
    windowed(options);
    return;
  }

  #[allow(unreachable_code)]
  give_up("this build has no client compiled in".to_owned())
}

fn window_conf() -> Conf {
  Conf {
    window_title: "Plaza Field Orders".to_owned(),
    window_width: 1100,
    window_height: 780,
    high_dpi: true,
    window_resizable: true,
    ..Default::default()
  }
}

#[cfg(all(feature = "client", feature = "websocket"))]
fn windowed(options: role::Options) {
  macroquad::Window::from_config(window_conf(), frame_loop(options));
}

/// One click, resolved against what the server said this unit may do.
///
/// Selection is local and everything else is an order: strike a ringed enemy,
/// march to an offered cell, tap the unit again to hold. Nothing is predicted;
/// the next snapshot is the outcome.
#[cfg(all(feature = "client", feature = "websocket"))]
fn resolve_click(client: &NetClient, selected: &mut Option<u8>, cell: Cell) -> Option<BattleOp> {
  let view = client.view.as_ref()?;
  if !client.commanding() {
    return None;
  }
  let army = client.my_army()?;
  let unit_at = view.units.iter().find(|u| u.at == cell);

  if let Some(sel) = *selected {
    if let Some(target) = unit_at.filter(|u| u.army != army)
      && client.orders_for(sel).is_some_and(|o| o.strike.contains(&target.id))
    {
      *selected = None;
      return Some(BattleOp::Strike { unit: sel, target: target.id });
    }
    if let Some(patient) = unit_at.filter(|u| u.army == army)
      && client.orders_for(sel).is_some_and(|o| o.heal.contains(&patient.id))
    {
      *selected = None;
      return Some(BattleOp::Heal { unit: sel, target: patient.id });
    }
    if unit_at.is_some_and(|u| u.id == sel) {
      *selected = None;
      return Some(BattleOp::Hold { unit: sel });
    }
    if client.orders_for(sel).is_some_and(|o| o.march.contains(&cell)) {
      // Selection survives the march: the unit may still act from where it
      // lands, and the next snapshot offers exactly that.
      return Some(BattleOp::Move { unit: sel, to: cell });
    }
  }

  // Selection is per squad: a teammate's unit renders as an ally and answers
  // to its own commander alone.
  match unit_at {
    Some(unit) if Some(unit.owner) == client.me && client.orders_for(unit.id).is_some() => {
      *selected = Some(unit.id);
      None
    }
    _ => {
      *selected = None;
      None
    }
  }
}

/// Everything on screen that is presentation rather than truth: floating
/// announcements, damage pops, hit flashes, eased health bars, and the phase
/// countdown. All of it is derived from moments the server sent; none of it
/// feeds back into an order.
#[cfg(all(feature = "client", feature = "websocket"))]
#[derive(Default)]
struct Fx {
  announcements: Vec<render::Announcement>,
  pops: Vec<render::Pop>,
  /// Unit id to the clock time its flash ends.
  flash: std::collections::HashMap<u8, u64>,
  /// Unit id to the health the bar currently shows, easing toward the truth.
  shown_hp: std::collections::HashMap<u8, f32>,
  /// The last authoritative hp seen, so a strike can say what it cost.
  last_hp: std::collections::HashMap<u8, i8>,
  /// When the current phase says it ends, on the local clock.
  deadline: Option<u64>,
}

#[cfg(all(feature = "client", feature = "websocket"))]
impl Fx {
  fn announce(&mut self, text: String, color: macroquad::color::Color, now: u64, life_ms: u64) {
    self.announcements.push(render::Announcement {
      text,
      color,
      born: now,
      life_ms,
    });
  }

  fn absorb(&mut self, moment: Moment, now: u64, my_army: Option<Army>) {
    match moment {
      Moment::Phase { phase, ends_in_ms } => {
        self.deadline = ends_in_ms.map(|ms| now + ms);
        if let BattlePhase::Command(army) = phase {
          let text = if my_army == Some(army) {
            "YOUR PHASE".to_owned()
          } else {
            format!("{army:?} COMMANDS").to_uppercase()
          };
          self.announce(text, render::army_color(army), now, 1500);
        }
      }
      Moment::Round { number } => {
        if number > 1 {
          self.announce(format!("ROUND {number}"), macroquad::color::WHITE, now, 1300);
        }
      }
      Moment::Struck {
        target,
        at,
        hp_left,
        felled,
        counter,
      } => {
        self.flash.insert(target, now + render::FLASH_LIFE_MS);
        if let Some(cell) = at {
          let damage = self.last_hp.get(&target).map(|prev| prev - hp_left.max(0)).filter(|d| *d > 0);
          let (text, color) = if felled {
            ("felled!".to_owned(), render::THREAT)
          } else {
            let text = damage.map(|d| format!("-{d}")).unwrap_or_else(|| format!("{hp_left} hp"));
            (text, if counter { render::COUNTER } else { render::THREAT })
          };
          self.pops.push(render::Pop {
            cell,
            text,
            color,
            born: now,
          });
        }
      }
      Moment::Healed { target, at, mended } => {
        self.flash.insert(target, now + render::FLASH_LIFE_MS);
        if let Some(cell) = at {
          self.pops.push(render::Pop {
            cell,
            text: format!("+{mended}"),
            color: render::MEND,
            born: now,
          });
        }
      }
      Moment::Over { winner } => {
        self.announce(
          format!("{winner:?} TAKES THE FIELD").to_uppercase(),
          render::army_color(winner),
          now,
          3000,
        );
      }
    }
  }

  fn update(&mut self, now: u64, dt: f32, view: Option<&plaza_example_field_orders::protocol::BattleView>) {
    self.announcements.retain(|a| now.saturating_sub(a.born) < a.life_ms);
    self.pops.retain(|p| now.saturating_sub(p.born) < render::POP_LIFE_MS);
    self.flash.retain(|_, until| *until > now);

    if let Some(view) = view {
      let ease = 1.0 - (-dt * 9.0).exp();
      for unit in &view.units {
        let shown = self.shown_hp.entry(unit.id).or_insert(unit.hp as f32);
        *shown += (unit.hp as f32 - *shown) * ease;
        self.last_hp.insert(unit.id, unit.hp);
      }
      // A redeploy reuses ids, so trackers for units off the board must go or
      // a fresh knight inherits a dead one's drained bar.
      self.shown_hp.retain(|id, _| view.units.iter().any(|u| u.id == *id));
      self.last_hp.retain(|id, _| view.units.iter().any(|u| u.id == *id));
    }
  }
}

#[cfg(all(feature = "client", feature = "websocket"))]
async fn frame_loop(options: role::Options) {
  #[cfg(feature = "server")]
  if options.role.runs_a_server() {
    let bind = options.bind.clone();
    let static_dir = options.static_dir.clone();
    std::thread::Builder::new()
      .name("field".to_owned())
      .spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        if let Err(e) = runtime.block_on(plaza_example_field_orders::net::host::serve(&bind, static_dir)) {
          eprintln!("field stopped: {e}");
        }
      })
      .expect("spawn the field thread");
    // The socket has to exist before the client connects to it.
    std::thread::sleep(std::time::Duration::from_millis(250));
  }

  let url = if options.role == Role::Client {
    options.connect.clone()
  } else {
    format!("ws://{}/ws", options.bind.replace("0.0.0.0", "127.0.0.1"))
  };

  let mut client = match NetClient::connect(&url) {
    Ok(client) => client,
    Err(e) => return give_up(format!("could not connect to {url}: {e}")),
  };

  let mut selected: Option<u8> = None;
  // Assigned from the absolute clock on the first frame, so there is no
  // starting value to read.
  let mut clock_ms;
  let mut fx = Fx::default();

  loop {
    let dt = get_frame_time().min(0.25);
    // Read absolutely rather than accumulated. Adding a truncated frame time
    // each frame runs the clock slow: 16.67ms counted as 16 loses 4% a second
    // at 60fps and 13.6% at 144, and every rate measured against it reads high
    // by the same amount. Truncating an absolute clock once is off by at most a
    // millisecond, for ever.
    clock_ms = (get_time() * 1000.0) as u64;

    client.poll(clock_ms);
    if !client.commanding() {
      selected = None;
    }

    let my_army = client.my_army();
    let moments: Vec<Moment> = client.moments.drain(..).collect();
    for moment in moments {
      fx.absorb(moment, clock_ms, my_army);
    }
    fx.update(clock_ms, dt, client.view.as_ref());

    clear_background(Color::new(0.05, 0.06, 0.08, 1.0));

    if let Some(view) = client.view.clone() {
      let board = render::Board::fit(view.terrain[0].len() as i8, view.terrain.len() as i8);
      render::draw_terrain(&board, &view.terrain);
      if let Some(sel) = selected
        && let Some(orders) = client.orders_for(sel).cloned()
      {
        render::draw_options(&board, &view, &orders);
      }
      render::draw_units(&board, &view, selected, &fx.shown_hp, &fx.flash, clock_ms);
      render::draw_pops(&board, clock_ms, &fx.pops);
      render::draw_banner(&view, client.me, my_army, fx.deadline.map(|d| d.saturating_sub(clock_ms)));
      render::draw_scoreboard(&board, &view, client.me);
      render::draw_announcements(clock_ms, &fx.announcements);

      if is_mouse_button_pressed(MouseButton::Left) {
        let (mx, my) = mouse_position();
        if let Some(cell) = board.cell_at(mx, my)
          && let Some(op) = resolve_click(&client, &mut selected, cell)
        {
          client.send(&op);
        }
      }
      if is_mouse_button_pressed(MouseButton::Right) {
        selected = None;
      }
      if is_key_pressed(KeyCode::E) && client.commanding() {
        client.send(&BattleOp::EndPhase);
        selected = None;
      }
    } else {
      let text = match &client.status {
        Status::Gone(reason) => reason.as_str(),
        _ => "waiting for the field",
      };
      let w = measure_text(text, None, 28, 1.0).width;
      draw_text(text, (screen_width() - w) * 0.5, screen_height() * 0.5, 28.0, GRAY);
    }

    let actions = ui::draw_panel(&client, &url);
    if actions.end_phase {
      client.send(&BattleOp::EndPhase);
      selected = None;
    }
    if let Some(choice) = actions.set_map {
      client.send(&BattleOp::SetMapSize(choice));
    }
    if actions.start_muster {
      client.send(&BattleOp::StartMuster);
    }
    egui_macroquad::draw();

    next_frame().await;
  }
}
