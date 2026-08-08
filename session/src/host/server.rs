//! The HTTP half of a listen server: bind a port, serve a browser client from
//! it, and put the WebSocket route on the same origin.

use actix_web::{middleware, web, App, HttpResponse, HttpServer};
use plaza_wire::frame::ProtocolVersion;

/// One page and its assets, served with the stamping that keeps a browser from
/// running yesterday's bundle against today's server.
#[derive(Clone, Debug)]
struct Page {
  dir: String,
  /// Assets whose URL is rewritten with a version stamp wherever the index
  /// mentions them.
  cache_busted: Vec<String>,
  /// The protocol version injected into the served index, or `None` to inject
  /// nothing.
  protocol: Option<ProtocolVersion>,
}

impl Page {
  /// The index, with every cache-busted asset's URL stamped with its own
  /// modification time.
  ///
  /// Read per request rather than at startup, so rebuilding the client reaches
  /// an already-running host without restarting it. That is the workflow this
  /// exists for: the bundle is a build product, so it does not rebuild when the
  /// server does.
  fn stamped_html(&self) -> Option<String> {
    let dir = std::path::Path::new(&self.dir);
    let mut html = std::fs::read_to_string(dir.join("index.html")).ok()?;
    for asset in &self.cache_busted {
      let stamp = std::fs::metadata(dir.join(asset))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
      html = html.replace(asset.as_str(), &format!("{asset}?v={stamp}"));
    }
    if let Some(protocol) = self.protocol {
      // Before `</head>`, so the value exists before any body script runs. A
      // static page has no build to bake a version into; being told at serve
      // time is the only way it can ever say what it speaks.
      let tag = format!("<script>window.PLAZA_PROTOCOL = {};</script>", protocol.0);
      match html.find("</head>") {
        Some(at) => html.insert_str(at, &tag),
        None => html.insert_str(0, &tag),
      }
    }
    Some(html)
  }

  fn index(&self) -> HttpResponse {
    let Some(html) = self.stamped_html() else {
      return HttpResponse::NotFound().body("index.html is missing from the served directory");
    };
    HttpResponse::Ok()
      // On *this* response above all. A cached index would keep quoting the old
      // stamp, which is the trap that makes cache busting look like it does not
      // work.
      .insert_header(("Cache-Control", "no-cache"))
      .content_type("text/html; charset=utf-8")
      .body(html)
  }
}

/// A local address somebody else could actually reach.
///
/// No dependency and no packets: connecting a UDP socket only picks a route, so
/// the kernel fills in the source address it would use. Printing it matters more
/// than it sounds, because "it is running" and "here is what to send your
/// friend" are different pieces of information and only one of them is useful.
pub fn lan_address() -> Option<String> {
  let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
  socket.connect("8.8.8.8:80").ok()?;
  Some(socket.local_addr().ok()?.ip().to_string())
}

/// Turns on console logging, once.
///
/// `plaza` and `plaza_session` are instrumented throughout and say useful things
/// about connections, presence and the controller loop, but `tracing` is silent
/// without a subscriber, and a server that logs nothing is indistinguishable
/// from a server that is not running.
///
/// A convenience for binaries. A library or an application with its own
/// subscriber should not call it; it is a no-op after the first call and after
/// any other global subscriber is installed. `RUST_LOG` overrides the default,
/// which is quiet enough to read and loud enough to show joins and leaves.
pub fn init_logging() {
  use std::sync::Once;
  static ONCE: Once = Once::new();
  ONCE.call_once(|| {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
      .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,actix_server=warn,actix_web=warn"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).with_target(false).try_init();
  });
}

/// The HTTP server a listen server runs behind.
///
/// It owns the parts that are the same in every such application and easy to get
/// subtly wrong: revalidation headers, the stamped index, the preflight on the
/// served directory, the banner, and leaving signal handling to the process. The
/// application supplies its own routes, which is where the WebSocket goes, so
/// none of this needs to know anything about the state being shared.
///
/// ```no_run
/// # use plaza_session::host::Host;
/// # use actix_web::web;
/// # async fn run() -> std::io::Result<()> {
/// Host::new("0.0.0.0:8080")
///   .serve_dir(Some("static".to_owned()))
///   .cache_bust("client.wasm")
///   .run(move |cfg| {
///     cfg.route("/ws", web::get().to(|| async { "" }));
///   })
///   .await
/// # }
/// ```
pub struct Host {
  bind: String,
  page: Option<Page>,
  announce: bool,
  ws_path: String,
}

impl Host {
  pub fn new(bind: impl Into<String>) -> Self {
    Self {
      bind: bind.into(),
      page: None,
      announce: true,
      ws_path: "/ws".to_owned(),
    }
  }

  /// Serves a browser client from this directory, or nothing when `None`.
  pub fn serve_dir(mut self, dir: Option<String>) -> Self {
    self.page = dir.map(|dir| Page {
      dir,
      cache_busted: Vec::new(),
      protocol: None,
    });
    self
  }

  /// Stamps this asset's URL with its modification time wherever the index
  /// mentions it. Call once per asset; has no effect without [`serve_dir`].
  ///
  /// [`serve_dir`]: Self::serve_dir
  pub fn cache_bust(mut self, asset: impl Into<String>) -> Self {
    if let Some(page) = &mut self.page {
      page.cache_busted.push(asset.into());
    }
    self
  }

  /// Injects `window.PLAZA_PROTOCOL` into the served index, so a page can
  /// announce a `Hello` and recognise a server it has outlived. Pass the same
  /// version the session declares. [`ProtocolVersion::UNKNOWN`] injects
  /// nothing, mirroring the session, which sends no `Hello` for it. Has no
  /// effect without [`serve_dir`](Self::serve_dir).
  pub fn protocol(mut self, protocol: ProtocolVersion) -> Self {
    if let Some(page) = &mut self.page {
      page.protocol = (protocol != ProtocolVersion::UNKNOWN).then_some(protocol);
    }
    self
  }

  /// The path quoted in the banner as the one clients join at. Cosmetic; the
  /// route itself is the application's to register.
  pub fn ws_path(mut self, path: impl Into<String>) -> Self {
    self.ws_path = path.into();
    self
  }

  /// Whether to print where to point a browser. On by default.
  pub fn announce(mut self, announce: bool) -> Self {
    self.announce = announce;
    self
  }

  /// Prints where to point a browser, unconditionally.
  ///
  /// Not through `tracing`: a log line only appears if somebody installed a
  /// subscriber and set a filter, and the first thing a person needs after
  /// starting a server is a URL. Making that depend on log configuration is how
  /// a working server looks broken.
  fn print_banner(&self) {
    let port = self.bind.rsplit(':').next().unwrap_or("8080");
    println!("\n  listening on {}", self.bind);
    if self.page.is_some() {
      println!("  play here:  http://127.0.0.1:{port}");
      if let Some(ip) = lan_address() {
        println!("  others at:  http://{ip}:{port}");
      }
    } else {
      println!("  no directory to serve, so there is no page to open.");
      println!("  clients can still join at ws://127.0.0.1:{port}{}", self.ws_path);
    }
    println!();
  }

  /// Binds and runs until the process ends.
  ///
  /// `configure` registers the application's own routes and shared data on every
  /// worker, which is where the WebSocket route goes. It runs once per worker
  /// thread, so anything it captures must be cheap to clone, an `Arc` typically.
  ///
  /// Blocks, so a windowed host calls it on a background thread with its own
  /// runtime while the frame loop keeps the main thread.
  pub async fn run<F>(self, configure: F) -> std::io::Result<()>
  where
    F: Fn(&mut web::ServiceConfig) + Send + Clone + 'static,
  {
    // Check the directory before binding. A missing index.html otherwise shows up
    // much later as a 404 in a browser, which looks like a routing bug rather
    // than a wrong path on the command line.
    if let Some(page) = &self.page {
      let path = std::path::Path::new(&page.dir);
      if !path.is_dir() {
        let cwd = std::env::current_dir().unwrap_or_default();
        return Err(std::io::Error::new(
          std::io::ErrorKind::NotFound,
          format!("{}: not a directory (relative to {cwd:?})", page.dir),
        ));
      }
      if !path.join("index.html").is_file() {
        return Err(std::io::Error::new(
          std::io::ErrorKind::NotFound,
          format!("{}: no index.html in it, so there would be nothing to open", page.dir),
        ));
      }
    }

    let page = self.page.clone();
    let server = HttpServer::new(move || {
      // `no-cache` means revalidate before reusing, not "do not store": with the
      // last-modified `actix_files` already sends, an unchanged asset still costs
      // a 304 and no bytes. Without it a wasm bundle is the one thing guaranteed
      // to go stale invisibly, because it is a build product and does not rebuild
      // when the server does.
      let app = App::new()
        .wrap(middleware::DefaultHeaders::new().add(("Cache-Control", "no-cache")))
        .configure(configure.clone());
      match &page {
        Some(page) => {
          let for_root = page.clone();
          let for_index = page.clone();
          app
            // Ahead of the file service, which would otherwise serve index.html
            // verbatim and unstamped.
            .route("/", web::get().to(move || {
              let page = for_root.clone();
              async move { page.index() }
            }))
            .route("/index.html", web::get().to(move || {
              let page = for_index.clone();
              async move { page.index() }
            }))
            .service(actix_files::Files::new("/", &page.dir).index_file("index.html"))
        }
        None => app,
      }
    })
    // Leave the signals to the process. A windowed host runs this on a background
    // thread while the frame loop owns the main one; if actix kept its own SIGINT
    // handler, Ctrl-C would start a graceful shutdown here, close the sockets, and
    // leave the window running and the controller spraying "connection closed" as
    // it kept ticking into dead links. With signals off, Ctrl-C ends the whole
    // process the way pressing it is meant to.
    .disable_signals()
    .bind(&self.bind)
    .map_err(|e| std::io::Error::new(e.kind(), format!("could not bind {}: {e}. Is something already using that port?", self.bind)))?;

    tracing::info!(bind = %self.bind, "listening");
    if self.announce {
      self.print_banner();
    }
    server.run().await
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn write_page(dir: &std::path::Path, html: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("index.html"), html).unwrap();
  }

  fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("plaza_host_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    dir
  }

  #[test]
  fn a_cache_busted_asset_is_stamped_with_its_own_modification_time() {
    // The whole point: the URL changes when the file does, so a browser cannot
    // reuse a bundle built before the wire changed.
    let dir = scratch("stamped");
    write_page(&dir, "<script src=\"client.wasm\"></script>");
    std::fs::write(dir.join("client.wasm"), b"\0asm").unwrap();

    let page = Page { dir: dir.to_string_lossy().into_owned(), cache_busted: vec!["client.wasm".to_owned()], protocol: None };
    let html = page.stamped_html().expect("the index is there");
    assert!(html.contains("client.wasm?v="), "the asset URL was not stamped: {html}");

    // And it moves when the file does, which is the property that actually
    // defeats the cache. A stamp that never changed would be decoration.
    let before = html;
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(dir.join("client.wasm"), b"\0asm\0").unwrap();
    let after = page.stamped_html().unwrap();
    assert_ne!(before, after, "rebuilding the bundle must change the URL the page asks for");

    assert_eq!(
      page.index().headers().get("Cache-Control").unwrap(),
      "no-cache",
      "the referencing page must revalidate, or it keeps quoting the old stamp"
    );
  }

  #[test]
  fn an_unmentioned_asset_is_left_alone() {
    // Stamping is opt-in per asset, so a page full of ordinary references is not
    // rewritten out from under the application.
    let dir = scratch("untouched");
    write_page(&dir, "<img src=\"logo.png\">");
    let page = Page { dir: dir.to_string_lossy().into_owned(), cache_busted: vec!["client.wasm".to_owned()], protocol: None };
    assert_eq!(page.stamped_html().unwrap(), "<img src=\"logo.png\">");
  }

  #[test]
  fn the_declared_protocol_is_stamped_into_the_head() {
    let dir = scratch("protocol");
    write_page(&dir, "<head><title>x</title></head><body></body>");
    let mut page = Page {
      dir: dir.to_string_lossy().into_owned(),
      cache_busted: Vec::new(),
      protocol: Some(ProtocolVersion(7)),
    };
    assert!(page
      .stamped_html()
      .unwrap()
      .contains("<script>window.PLAZA_PROTOCOL = 7;</script></head>"));

    page.protocol = None;
    assert!(!page.stamped_html().unwrap().contains("PLAZA_PROTOCOL"));
  }

  #[test]
  fn a_missing_index_is_a_404_rather_than_a_panic() {
    let dir = scratch("empty");
    std::fs::create_dir_all(&dir).unwrap();
    let page = Page { dir: dir.to_string_lossy().into_owned(), cache_busted: Vec::new(), protocol: None };
    assert_eq!(page.index().status(), actix_web::http::StatusCode::NOT_FOUND);
  }

  #[actix_web::test]
  async fn a_served_directory_that_does_not_exist_fails_before_binding() {
    // Otherwise it shows up much later as a 404 in a browser, which looks like a
    // routing bug rather than a wrong path on the command line.
    let err = Host::new("127.0.0.1:0")
      .serve_dir(Some("/definitely/not/a/directory".to_owned()))
      .announce(false)
      .run(|_| {})
      .await
      .expect_err("a missing directory must be refused");
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    assert!(err.to_string().contains("not a directory"), "{err}");
  }

  #[actix_web::test]
  async fn a_directory_without_an_index_fails_before_binding() {
    let dir = scratch("noindex");
    std::fs::create_dir_all(&dir).unwrap();
    let err = Host::new("127.0.0.1:0")
      .serve_dir(Some(dir.to_string_lossy().into_owned()))
      .announce(false)
      .run(|_| {})
      .await
      .expect_err("a directory with nothing to open must be refused");
    assert!(err.to_string().contains("no index.html"), "{err}");
  }
}
