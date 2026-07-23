//! What this process is: one argument, four answers.
//!
//! A scatter of booleans (`--host`, `--headless`, `--connect`) lets you ask for
//! combinations that cannot exist, and then has to reject them one pair at a
//! time. The combinations here are not independent, so one enum says what is
//! possible and nothing else needs checking.
//!
//! A build may not support every role. `--role headless` needs the `server`
//! feature, `--role client` needs `websocket`, and a browser can only ever
//! join. Asking for a role this build lacks is answered by naming the feature,
//! not by a panic, because "unknown option" is a much worse message than "this
//! build has no server in it".

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
  /// Server only, no window. The deployable.
  Headless,
  /// Server with a window and the full control panel, but no hole of your own.
  /// For watching an arena, or for driving the settings while others play.
  Observer,
  /// Hosts and plays. The default on a desktop, and what the offline demo has
  /// always been.
  Host,
  /// Joins somebody else's arena. The only role a browser can take.
  Client,
}

impl Role {
  pub fn runs_a_server(self) -> bool {
    matches!(self, Role::Headless | Role::Observer | Role::Host)
  }

  pub fn opens_a_window(self) -> bool {
    !matches!(self, Role::Headless)
  }

  /// Whether this process drives a hole of its own.
  pub fn plays(self) -> bool {
    matches!(self, Role::Host | Role::Client)
  }

  fn parse(text: &str) -> Option<Self> {
    match text {
      "headless" | "server" => Some(Role::Headless),
      "observer" | "observe" => Some(Role::Observer),
      "host" => Some(Role::Host),
      "client" | "join" => Some(Role::Client),
      _ => None,
    }
  }
}

impl fmt::Display for Role {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let name = match self {
      Role::Headless => "headless",
      Role::Observer => "observer",
      Role::Host => "host",
      Role::Client => "client",
    };
    f.write_str(name)
  }
}

#[derive(Clone, Debug)]
pub struct Options {
  pub role: Role,
  /// What to listen on when hosting.
  pub bind: String,
  /// Where to connect when joining. Defaults to the local host's own port, so
  /// `--role client` with nothing else joins a host on this machine.
  pub connect: String,
  /// Directory to serve the browser client from.
  ///
  /// Defaults to this crate's own `static/`, baked in as an **absolute** path at
  /// compile time. A relative default would resolve against the working
  /// directory, so running from anywhere but the repository root would serve
  /// nothing and answer every request with a 404: a server that looks healthy
  /// and is not. `--serve` overrides it; `--no-serve` turns it off.
  pub static_dir: Option<String>,
}

/// This crate's `static/`, as an absolute path.
pub const DEFAULT_STATIC_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/static");

impl Default for Role {
  fn default() -> Self {
    // A browser can only ever join: it cannot accept incoming connections and
    // its build has no server in it. Defaulting to `Host` there meant the wasm
    // client asked for a role it could not perform, failed the feature check,
    // and called `process::exit`, which in wasm is a trap. The page loaded and
    // then died with `unreachable executed`.
    if cfg!(target_arch = "wasm32") { Role::Client } else { Role::Host }
  }
}

impl Default for Options {
  fn default() -> Self {
    Self {
      role: Role::default(),
      // All interfaces, because the entire point is that somebody else can
      // reach it. A demo that only ever listened on loopback would be a
      // single-player game with extra steps.
      bind: "0.0.0.0:8080".to_owned(),
      connect: "ws://127.0.0.1:8080/ws".to_owned(),
      static_dir: Some(DEFAULT_STATIC_DIR.to_owned()),
    }
  }
}

pub const USAGE: &str = "\
blackhole_playground

  --role <headless|observer|host|client>   what this process is (default: host)
  --bind <addr:port>                       what to listen on   (default: 0.0.0.0:8080)
  --connect <ws url>                       what to join        (default: ws://127.0.0.1:8080/ws)
  --serve <dir>                            serve a browser client from this directory
                                           (defaults to this crate's static/)
  --no-serve                               do not serve a page at all
  --help

roles
  headless   server only, no window. The thing you deploy.
  observer   server with a window and every control, but no hole of your own.
  host       hosts and plays. Others join at the address you are bound to.
  client     joins somebody else's arena.
";

/// Parses argv, or returns a message to print and exit on.
pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Options, String> {
  let mut options = Options::default();
  let mut args = args.into_iter().skip(1);

  while let Some(arg) = args.next() {
    let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value\n\n{USAGE}"));
    match arg.as_str() {
      "--role" => {
        let text = value("--role")?;
        options.role = Role::parse(&text).ok_or_else(|| format!("unknown role `{text}`\n\n{USAGE}"))?;
      }
      "--bind" => options.bind = value("--bind")?,
      "--connect" => options.connect = value("--connect")?,
      "--serve" => options.static_dir = Some(value("--serve")?),
      "--no-serve" => options.static_dir = None,
      "--help" | "-h" => return Err(USAGE.to_owned()),
      other => return Err(format!("unknown option `{other}`\n\n{USAGE}")),
    }
  }
  Ok(options)
}

/// Rejects a role this build cannot perform, naming the feature that is missing.
pub fn check_supported(role: Role) -> Result<(), String> {
  if role.runs_a_server() && !cfg!(feature = "server") {
    return Err(format!("`--role {role}` needs a server, and this build has none. Rebuild with `--features server`."));
  }
  if role == Role::Client && !cfg!(feature = "websocket") {
    return Err(format!("`--role {role}` needs a socket, and this build has none. Rebuild with `--features websocket`."));
  }
  if role.plays() && !cfg!(feature = "client") {
    return Err(format!("`--role {role}` needs a client, and this build has none. Rebuild with `--features client`."));
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn args(rest: &[&str]) -> Vec<String> {
    std::iter::once("blackhole".to_owned()).chain(rest.iter().map(|s| (*s).to_owned())).collect()
  }

  #[test]
  fn the_default_is_the_demo_that_already_existed() {
    // Running it with no arguments must still be the thing it has always been,
    // and now also a host nobody has joined.
    let options = parse(args(&[])).unwrap();
    assert_eq!(options.role, Role::Host);
    assert!(options.role.plays() && options.role.runs_a_server() && options.role.opens_a_window());
  }

  #[test]
  fn a_browser_defaults_to_the_only_role_it_can_perform() {
    // Regression for a page that loaded and immediately trapped. A wasm build
    // has no server and cannot listen, so defaulting to `Host` made it fail its
    // own feature check and call `process::exit`, which is `unreachable` in
    // wasm. The browser saw `RuntimeError: unreachable executed` and nothing
    // else.
    let expected = if cfg!(target_arch = "wasm32") { Role::Client } else { Role::Host };
    assert_eq!(Role::default(), expected);
    assert!(!Role::Client.runs_a_server(), "the browser role must never need a server");
  }

  #[test]
  fn each_role_answers_the_three_questions_differently() {
    // The reason this is one enum and not three booleans: only four of the eight
    // combinations mean anything.
    let cases = [
      (Role::Headless, (true, false, false)),
      (Role::Observer, (true, true, false)),
      (Role::Host, (true, true, true)),
      (Role::Client, (false, true, true)),
    ];
    for (role, (server, window, plays)) in cases {
      assert_eq!(role.runs_a_server(), server, "{role} server");
      assert_eq!(role.opens_a_window(), window, "{role} window");
      assert_eq!(role.plays(), plays, "{role} plays");
    }
  }

  #[test]
  fn aliases_exist_for_the_names_people_actually_type() {
    assert_eq!(parse(args(&["--role", "server"])).unwrap().role, Role::Headless);
    assert_eq!(parse(args(&["--role", "join"])).unwrap().role, Role::Client);
  }

  #[test]
  fn a_bad_role_explains_itself_rather_than_failing_silently() {
    let err = parse(args(&["--role", "sideways"])).unwrap_err();
    assert!(err.contains("unknown role `sideways`"));
    assert!(err.contains("headless"), "the message lists what is valid");
  }

  #[test]
  fn the_page_is_served_from_an_absolute_path_by_default() {
    // The whole class of "server runs, everything 404s" bug: a relative default
    // resolves against the working directory, so it works from the repository
    // root and silently serves nothing from anywhere else.
    let dir = parse(args(&[])).unwrap().static_dir.expect("a page by default");
    assert!(std::path::Path::new(&dir).is_absolute(), "{dir} must not depend on the working directory");
    assert!(dir.ends_with("/static"));
  }

  #[test]
  fn serving_can_be_turned_off_and_overridden() {
    assert_eq!(parse(args(&["--no-serve"])).unwrap().static_dir, None);
    assert_eq!(parse(args(&["--serve", "/tmp/x"])).unwrap().static_dir.as_deref(), Some("/tmp/x"));
  }

  #[test]
  fn a_flag_without_its_value_is_an_error_not_a_default() {
    assert!(parse(args(&["--connect"])).unwrap_err().contains("--connect needs a value"));
  }
}
