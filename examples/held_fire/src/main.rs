//! Frame loop: pick a unit, march or shoot or watch, and answer the offer
//! before its window closes.

#[cfg(all(feature = "client", feature = "websocket"))]
mod render;
#[cfg(all(feature = "client", feature = "websocket"))]
mod ui;

#[cfg(all(feature = "client", feature = "websocket"))]
use macroquad::prelude::*;
use held_fire::role;
use held_fire::role::Role;

#[cfg(all(feature = "client", feature = "websocket"))]
use held_fire::net::client::{Moment, NetClient, Status};

/// Reports a fatal misconfiguration. Never `process::exit` on wasm: the call
/// traps and the page dies with `unreachable executed` and no reason.
fn give_up(message: String) {
  if cfg!(target_arch = "wasm32") {
    println!("{message}");
  } else {
    eprintln!("{message}");
    std::process::exit(2);
  }
}

fn main() {
  let options = match role::parse(std::env::args()) {
    Ok(options) => options,
    Err(message) => return give_up(message),
  };

  if let Err(message) = role::check_supported(options.role) {
    return give_up(message);
  }
  if options.role == Role::Observer {
    return give_up("held_fire has no observer flag: join as a client, and you watch the whole board once both sides are commanded".to_owned());
  }

  #[cfg(feature = "server")]
  if options.role == Role::Headless {
    let result = tokio::runtime::Runtime::new()
      .expect("tokio runtime")
      .block_on(held_fire::net::host::serve(&options.bind, options.static_dir.clone()));
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

#[cfg(all(feature = "client", feature = "websocket"))]
fn window_conf() -> Conf {
  Conf {
    window_title: "Plaza Held Fire".to_owned(),
    window_width: 1100,
    window_height: 700,
    high_dpi: true,
    window_resizable: true,
    ..Default::default()
  }
}

#[cfg(all(feature = "client", feature = "websocket"))]
fn windowed(options: role::Options) {
  macroquad::Window::from_config(window_conf(), frame_loop(options));
}

#[cfg(all(feature = "client", feature = "websocket"))]
async fn frame_loop(options: role::Options) {
  use held_fire::protocol::{Order, ACT_LIMIT_MS, STEP_MS};
  use held_fire::sight;

  #[cfg(feature = "server")]
  if options.role.runs_a_server() {
    let bind = options.bind.clone();
    let static_dir = options.static_dir.clone();
    std::thread::Builder::new()
      .name("skirmish".to_owned())
      .spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        if let Err(e) = runtime.block_on(held_fire::net::host::serve(&bind, static_dir)) {
          eprintln!("skirmish stopped: {e}");
        }
      })
      .expect("spawn the skirmish thread");
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

  let mut clock_ms;
  let mut effects = render::Effects::default();
  let mut hud = render::Hud::default();
  let mut had_activation = false;

  loop {
    clock_ms = (get_time() * 1000.0) as u64;
    client.poll(clock_ms);

    let moments: Vec<Moment> = client.moments.drain(..).collect();
    for moment in moments {
      match moment {
        Moment::Shot { shooter, target, felled, overwatch } => {
          let from = client
            .view
            .as_ref()
            .and_then(|v| {
              v.yours
                .iter()
                .find(|u| u.id == shooter)
                .map(|u| u.at)
                .or_else(|| v.seen.iter().find(|s| s.id == shooter).map(|s| s.at))
            });
          let to = client.view.as_ref().and_then(|v| {
            v.yours
              .iter()
              .find(|u| u.id == target)
              .map(|u| u.at)
              .or_else(|| v.seen.iter().find(|s| s.id == target).map(|s| s.at))
          });
          if let (Some(from), Some(to)) = (from, to) {
            effects.shot(from, to, overwatch, clock_ms);
          }
          if felled {
            effects.banner(format!("unit {target} falls"), clock_ms);
          }
        }
        Moment::OfferOpened(offer) => hud.offer = Some((offer, clock_ms + STEP_MS)),
        Moment::BattleOver { winner } => {
          effects.banner(
            format!("the {} side takes the field", ["blue", "red"][winner as usize % 2]),
            clock_ms,
          );
          hud.selected = None;
          hud.offer = None;
        }
        Moment::BattleStarted | Moment::RoundStarted(_) => {
          hud.selected = None;
        }
        _ => {}
      }
    }
    effects.sweep(clock_ms);

    // The offer closes with its window whether or not anybody clicked.
    if hud.offer.as_ref().is_some_and(|(_, closes)| *closes <= clock_ms)
      || client.my_offer().is_none()
    {
      if client.my_offer().is_none() {
        hud.offer = None;
      }
    }
    if let Some(offer) = client.my_offer()
      && hud.offer.is_none()
    {
      hud.offer = Some((offer, clock_ms + STEP_MS));
    }

    let acting = client.my_activation();
    if acting && !had_activation {
      hud.act_deadline = Some(clock_ms + ACT_LIMIT_MS);
    }
    if !acting {
      hud.act_deadline = None;
      hud.selected = None;
    }
    had_activation = acting;

    // Clicks, against the same layout the drawing uses.
    if is_mouse_button_pressed(MouseButton::Left) {
      let mouse = vec2(mouse_position().0, mouse_position().1);

      if let Some((offer, _)) = hud.offer {
        let (fire, hold) = render::offer_buttons();
        if fire.contains(mouse) {
          client.answer(offer.watcher, true);
          hud.offer = None;
        } else if hold.contains(mouse) {
          client.answer(offer.watcher, false);
          hud.offer = None;
        }
      } else if acting {
        let (watch, cancel) = render::watch_buttons();
        if let (Some(selected), true) = (hud.selected, watch.contains(mouse)) {
          client.act(Order::Overwatch { unit: selected });
          hud.selected = None;
        } else if hud.selected.is_some() && cancel.contains(mouse) {
          hud.selected = None;
        } else if let Some(cell) = render::cell_at(mouse) {
          let view = client.view.clone().expect("acting implies a view");
          let side = client.my_side.expect("acting implies a side");
          let own = view
            .yours
            .iter()
            .find(|u| u.alive && !u.acted && u.at == cell && u.side == side);
          let enemy = view.seen.iter().find(|s| s.alive && s.at == cell);
          match (own, enemy, hud.selected) {
            (Some(unit), _, _) => hud.selected = Some(unit.id),
            (None, Some(target), Some(selected)) => {
              let me = view.yours.iter().find(|u| u.id == selected);
              if me.is_some_and(|u| sight::sees(u.at, target.at)) {
                client.act(Order::Shoot {
                  unit: selected,
                  target: target.id,
                });
                hud.selected = None;
              }
            }
            (None, None, Some(selected)) => {
              client.act(Order::March { unit: selected, to: cell });
              hud.selected = None;
            }
            _ => {}
          }
        }
      }
    }
    if is_mouse_button_pressed(MouseButton::Right) || is_key_pressed(KeyCode::Escape) {
      hud.selected = None;
    }

    clear_background(Color::new(0.06, 0.06, 0.08, 1.0));
    if client.view.is_some() {
      render::draw_scene(&client, &effects, &hud, clock_ms);
    } else {
      let text = match &client.status {
        Status::Gone(reason) => reason.as_str(),
        _ => "waiting for the field",
      };
      let w = measure_text(text, None, 28, 1.0).width;
      draw_text(text, (screen_width() - w) * 0.5, screen_height() * 0.5, 28.0, GRAY);
    }
    ui::draw_panel(&client, &url);
    egui_macroquad::draw();

    next_frame().await;
  }
}
