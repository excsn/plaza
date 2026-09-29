//! The role of a listen-server process (one argument with four possible values)
//! and the parsing that turns argv into it.
//!
//! A listen server is a process that holds the authority and may also play, so
//! each process has one of a few roles. Describing that with separate booleans
//! (`--host`, `--headless`, `--connect`) lets a caller request combinations that
//! cannot exist, which then have to be rejected a pair at a time. A [`Role`] can
//! only hold a combination that exists, so nothing else needs checking.
//!
//! # Why this is a separate crate outside the library
//!
//! Both halves of a listen server need this vocabulary: the server parses
//! `--role headless` and the browser client it serves needs to know it can only
//! be a [`Role::Client`]. The client is a wasm bundle and must not pull in an
//! HTTP server and an async runtime to name its own role. So this cannot live
//! beside the hosting code in `plaza_session` and it has no dependencies at all.
//!
//! # What else lives here
//!
//! [`touch`], behind a feature. Every playground ships a browser build and is
//! therefore reachable from a phone, so the examples that would otherwise need
//! a keyboard share one set of on-screen controls. Nothing else lives here.
//!
//! The wasm constraint only rules out `plaza_session`. It does not make role
//! parsing part of the published library. An application of any size will
//! parse arguments its own way (clap, a config file or the environment it is
//! deployed into). The part that generalises is that only four of the eight
//! role combinations mean anything. The parsing around it is scaffolding shared
//! between examples. The reusable half of a listen server is
//! `plaza_session::host::Host`, which is where the HTTP layer lives.

#[cfg(feature = "touch")]
pub mod touch;

use std::fmt;


/// What a process is, in a deployment where the authority and a player can be
/// the same program.
///
/// One enum rather than three booleans because only four of the eight
/// combinations mean anything and separate flags would have to reject the
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
    // asks for a role it cannot perform, fails its own feature check and calls
    // `process::exit`, which in wasm is a trap. The page loads and then stops
    // with `unreachable executed` and no reason given.
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
  /// repository root and answers every request with a 404 from anywhere else,
  /// while looking healthy.
  pub static_dir: Option<String>,
  /// How many rooms a host should run, for a game that has more than one.
  ///
  /// One by default, because rooms are expensive. Each room is a whole
  /// simulation, so a local run of a many-entity game pays for every extra one
  /// while a single player is using a single arena. More rooms are worth it
  /// when real people with real connections arrive, which is a deployment
  /// decision rather than a property of the example.
  pub rooms: usize,
}

impl Default for Options {
  fn default() -> Self {
    Self {
      role: Role::default(),
      // All interfaces, so somebody else can reach it.
      bind: "0.0.0.0:8080".to_owned(),
      connect: "ws://127.0.0.1:8080/ws".to_owned(),
      static_dir: None,
      rooms: 1,
    }
  }
}

/// Which roles a *build* can perform, as opposed to which roles exist.
///
/// Feature flags are per crate, so a library cannot read the application's with
/// `cfg!`. The application passes its own answers in and gets an error message
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
  --rooms <n>                              arenas to run       (default: 1)
  --help

rooms
  One arena is right for a local run: each is a whole simulation, so extra ones
  cost a machine that has one player on it. Run several when real connections
  arrive and the spread of their latency is worth having somewhere to put.

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
      "--rooms" => {
        options.rooms = value("--rooms")?
          .parse()
          .map_err(|_| format!("--rooms wants a number\n\n{usage}"))?;
        if options.rooms == 0 {
          return Err(format!("--rooms must be at least 1\n\n{usage}"));
        }
      }
      "--help" | "-h" => return Err(usage),
      other => return Err(format!("unknown option `{other}`\n\n{usage}")),
    }
  }
  Ok(options)
}

/// Rejects a role this build cannot perform, naming the feature that is missing
/// rather than reporting an unknown option or panicking.
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

/// Expands to a playground's whole `role` module: the crate's defaults, `parse`,
/// and `check_supported`.
///
/// A macro rather than library functions because two of the pieces can only be
/// evaluated in the leaf crate. `cfg!(feature = ...)` reads the features of the
/// crate being compiled, so a check written as code in this library would read
/// *this* crate's features and approve a role the binary has no code for.
/// Likewise `env!("CARGO_MANIFEST_DIR")` has to name the leaf crate's
/// `static/` rather than this one's. Expanding at the call site makes both read
/// the leaf crate.
///
/// ```ignore
/// // src/role.rs, the whole file:
/// playground_common::playground_role!(port: 8097);   // or `!()` for 8080
/// ```
#[macro_export]
macro_rules! playground_role {
  ($(port: $port:literal)?) => {
    pub use $crate::{usage, Options, Role};

    /// This crate's `static/`, as an absolute path.
    ///
    /// Absolute and baked in at compile time. A relative default resolves
    /// against the working directory, so running from anywhere but the
    /// repository root would serve nothing and answer every request with a
    /// 404 while looking healthy.
    pub const DEFAULT_STATIC_DIR: &str = ::core::concat!(::core::env!("CARGO_MANIFEST_DIR"), "/static");

    /// Where this example starts before the command line has its say.
    pub fn defaults() -> $crate::Options {
      $crate::Options {
        static_dir: ::core::option::Option::Some(DEFAULT_STATIC_DIR.to_owned()),
        $(
          bind: ::core::concat!("0.0.0.0:", $port).to_owned(),
          connect: ::core::concat!("ws://127.0.0.1:", $port, "/ws").to_owned(),
        )?
        ..$crate::Options::default()
      }
    }

    /// Parses argv, or returns a message to print and exit on.
    pub fn parse<I: ::core::iter::IntoIterator<Item = ::std::string::String>>(
      args: I,
    ) -> ::core::result::Result<$crate::Options, ::std::string::String> {
      $crate::parse(args, defaults())
    }

    /// Rejects a role this build cannot perform, naming the feature that is
    /// missing. The `cfg!`s read this crate's features, which is the reason
    /// this is macro-expanded here rather than written in the library.
    pub fn check_supported(role: $crate::Role) -> ::core::result::Result<(), ::std::string::String> {
      $crate::check_supported(
        role,
        $crate::Support {
          server: ::core::cfg!(feature = "server"),
          websocket: ::core::cfg!(feature = "websocket"),
          client: ::core::cfg!(feature = "client"),
        },
      )
    }
  };
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
    // Only four of the eight combinations mean anything, so this is one enum
    // rather than three booleans.
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
