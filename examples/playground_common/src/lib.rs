//! What a listen-server process **is**: one argument with four answers, and the
//! parsing that turns argv into one.
//!
//! A listen server is a process that holds the authority and may also play. That
//! makes "what is this process" a real question with a small number of real
//! answers, and asking it with a scatter of booleans (`--host`, `--headless`,
//! `--connect`) lets a caller request combinations that cannot exist, which then
//! have to be rejected a pair at a time. One [`Role`] says what is possible and
//! nothing else needs checking.
//!
//! # Why this is its own crate, and why it is not part of the library
//!
//! Both halves of a listen server need this vocabulary: the server parses
//! `--role headless`, and the browser client it serves needs to know it can only
//! ever be a [`Role::Client`]. One of those halves is a wasm bundle, and it must
//! not inherit an HTTP server and an async runtime to learn the name of its own
//! role. So it cannot live beside the hosting code in `plaza_session`, and it has
//! no dependencies at all.
//!
//! That is an argument about where it *cannot* go, though, not an argument that
//! it belongs in the published library. Argument parsing is an opinion, and an
//! application of any size will have its own (clap, or a config file, or an
//! environment it is deployed into). What generalises is the observation that
//! only four of the eight role combinations mean anything; the parsing around it
//! is scaffolding, and scaffolding shared between two examples is exactly what
//! this is. The genuinely reusable half of a listen server is
//! `plaza_session::host::Host`, which is where the HTTP layer lives.

use std::fmt;


/// What a process is, in a deployment where the authority and a player can be
/// the same program.
///
/// One enum rather than three booleans because only four of the eight
/// combinations mean anything, and a scatter of flags has to reject the
/// impossible ones a pair at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
  /// Server only, no window. The thing you deploy.
  Headless,
  /// Server with a window and every control, but no player of its own. For
  /// watching, or for driving the settings while others play.
  Observer,
  /// Hosts and plays. The usual desktop default.
  Host,
  /// Joins somebody else's server. The only role a browser can take.
  Client,
}

impl Role {
  pub fn runs_a_server(self) -> bool {
    matches!(self, Role::Headless | Role::Observer | Role::Host)
  }

  pub fn opens_a_window(self) -> bool {
    !matches!(self, Role::Headless)
  }

  /// Whether this process drives a participant of its own.
  pub fn plays(self) -> bool {
    matches!(self, Role::Host | Role::Client)
  }

  /// Parses a role name, including the aliases people actually type.
  pub fn parse(text: &str) -> Option<Self> {
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

impl Default for Role {
  fn default() -> Self {
    // A browser can only ever join: it cannot accept incoming connections and its
    // build has no server in it. Defaulting to `Host` there means the wasm client
    // asks for a role it cannot perform, fails its own feature check, and calls
    // `process::exit`, which in wasm is a trap. The page loads and then dies with
    // `unreachable executed` and no reason for it.
    if cfg!(target_arch = "wasm32") { Role::Client } else { Role::Host }
  }
}

/// What was asked for on the command line.
#[derive(Clone, Debug)]
pub struct Options {
  pub role: Role,
  /// What to listen on when hosting.
  pub bind: String,
  /// Where to connect when joining.
  pub connect: String,
  /// Directory to serve a browser client from, or `None` for no page at all.
  ///
  /// Give this an **absolute** default, baked in at compile time with
  /// `concat!(env!("CARGO_MANIFEST_DIR"), "/static")`. A relative default
  /// resolves against the working directory, so the server works from the
  /// repository root and answers every request with a 404 from anywhere else: a
  /// server that looks healthy and is not.
  pub static_dir: Option<String>,
}

impl Default for Options {
  fn default() -> Self {
    Self {
      role: Role::default(),
      // All interfaces, because the entire point is that somebody else can reach
      // it. A demo that only ever listened on loopback would be single player
      // with extra steps.
      bind: "0.0.0.0:8080".to_owned(),
      connect: "ws://127.0.0.1:8080/ws".to_owned(),
      static_dir: None,
    }
  }
}

/// Which roles a *build* can perform, which is not the same question as which
/// roles exist.
///
/// Feature flags are per crate, so a library cannot read the application's with
/// `cfg!`. The application passes its own answers in, and gets an error message
/// that names the missing feature rather than a panic.
#[derive(Clone, Copy, Debug)]
pub struct Support {
  /// Whether an authoritative server is compiled in.
  pub server: bool,
  /// Whether a client socket is compiled in.
  pub websocket: bool,
  /// Whether a participant of this process's own is compiled in.
  pub client: bool,
}

impl Default for Support {
  fn default() -> Self {
    Self { server: true, websocket: true, client: true }
  }
}

/// The usage text, for a program of the given name.
pub fn usage(program: &str) -> String {
  format!(
    "\
{program}

  --role <headless|observer|host|client>   what this process is (default: host)
  --bind <addr:port>                       what to listen on   (default: 0.0.0.0:8080)
  --connect <ws url>                       what to join        (default: ws://127.0.0.1:8080/ws)
  --serve <dir>                            serve a browser client from this directory
  --no-serve                               do not serve a page at all
  --help

roles
  headless   server only, no window. The thing you deploy.
  observer   server with a window and every control, but no player of your own.
  host       hosts and plays. Others join at the address you are bound to.
  client     joins somebody else's server.
"
  )
}

/// Parses argv over a set of defaults, or returns a message to print and exit on.
///
/// `defaults` carries the application's own choices for bind, connect and the
/// static directory; anything on the command line overrides them.
pub fn parse<I: IntoIterator<Item = String>>(args: I, defaults: Options) -> Result<Options, String> {
  let mut options = defaults;
  let mut args = args.into_iter();
  let program = args.next().unwrap_or_else(|| "server".to_owned());
  let usage = usage(&program);

  while let Some(arg) = args.next() {
    let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value\n\n{usage}"));
    match arg.as_str() {
      "--role" => {
        let text = value("--role")?;
        options.role = Role::parse(&text).ok_or_else(|| format!("unknown role `{text}`\n\n{usage}"))?;
      }
      "--bind" => options.bind = value("--bind")?,
      "--connect" => options.connect = value("--connect")?,
      "--serve" => options.static_dir = Some(value("--serve")?),
      "--no-serve" => options.static_dir = None,
      "--help" | "-h" => return Err(usage),
      other => return Err(format!("unknown option `{other}`\n\n{usage}")),
    }
  }
  Ok(options)
}

/// Rejects a role this build cannot perform, naming the feature that is missing.
///
/// "This build has no server in it" is a far better message than "unknown
/// option", and much better than a panic.
pub fn check_supported(role: Role, support: Support) -> Result<(), String> {
  if role.runs_a_server() && !support.server {
    return Err(format!("`--role {role}` needs a server, and this build has none. Rebuild with `--features server`."));
  }
  if role == Role::Client && !support.websocket {
    return Err(format!("`--role {role}` needs a socket, and this build has none. Rebuild with `--features websocket`."));
  }
  if role.plays() && !support.client {
    return Err(format!("`--role {role}` needs a client, and this build has none. Rebuild with `--features client`."));
  }
  Ok(())
}


#[cfg(test)]
mod tests {
  use super::*;

  fn args(rest: &[&str]) -> Vec<String> {
    std::iter::once("demo".to_owned()).chain(rest.iter().map(|s| (*s).to_owned())).collect()
  }

  fn defaults() -> Options {
    Options { static_dir: Some("/somewhere/static".to_owned()), ..Options::default() }
  }

  #[test]
  fn the_default_is_hosting_and_playing() {
    let options = parse(args(&[]), defaults()).unwrap();
    assert_eq!(options.role, Role::default());
  }

  #[test]
  fn a_browser_defaults_to_the_only_role_it_can_perform() {
    // Regression for a page that loaded and immediately trapped. A wasm build has
    // no server and cannot listen, so defaulting to `Host` made it fail its own
    // feature check and call `process::exit`, which is `unreachable` in wasm. The
    // browser saw `RuntimeError: unreachable executed` and nothing else.
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
    assert_eq!(parse(args(&["--role", "server"]), defaults()).unwrap().role, Role::Headless);
    assert_eq!(parse(args(&["--role", "join"]), defaults()).unwrap().role, Role::Client);
  }

  #[test]
  fn a_bad_role_explains_itself_rather_than_failing_silently() {
    let err = parse(args(&["--role", "sideways"]), defaults()).unwrap_err();
    assert!(err.contains("unknown role `sideways`"));
    assert!(err.contains("headless"), "the message lists what is valid");
  }

  #[test]
  fn serving_can_be_turned_off_and_overridden() {
    assert_eq!(parse(args(&["--no-serve"]), defaults()).unwrap().static_dir, None);
    assert_eq!(parse(args(&["--serve", "/tmp/x"]), defaults()).unwrap().static_dir.as_deref(), Some("/tmp/x"));
  }

  #[test]
  fn a_flag_without_its_value_is_an_error_not_a_default() {
    assert!(parse(args(&["--connect"]), defaults()).unwrap_err().contains("--connect needs a value"));
  }

  #[test]
  fn the_usage_names_the_program_it_was_run_as() {
    assert!(usage("horde_playground").starts_with("horde_playground"));
    let err = parse(args(&["--help"]), defaults()).unwrap_err();
    assert!(err.starts_with("demo"), "the usage quotes argv[0]: {err}");
  }

  #[test]
  fn a_role_this_build_cannot_perform_names_the_missing_feature() {
    // The application owns the feature flags, so it answers rather than the
    // library guessing with a `cfg!` that would read its own.
    let no_server = Support { server: false, ..Support::default() };
    let err = check_supported(Role::Headless, no_server).unwrap_err();
    assert!(err.contains("--features server"), "the message says how to fix it: {err}");
    assert!(check_supported(Role::Client, no_server).is_ok(), "joining needs no server");
  }
}
