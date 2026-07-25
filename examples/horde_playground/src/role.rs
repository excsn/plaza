//! What this process is: one argument, four answers.
//!
//! The enum, the parsing and the feature check live in [`playground_common`], because
//! they are the same in every listen server and were previously the same in this
//! repository twice over. That crate has no dependencies, which is what lets the
//! wasm client share one definition of the four roles with the server it joins
//! rather than carrying a second copy.
//!
//! What is left here is only what is this crate's own: the defaults it starts
//! from, and the feature flags it was built with, which a library cannot read
//! for it.

pub use playground_common::{usage, Options, Role};

use playground_common::Support;

/// This crate's `static/`, as an absolute path.
///
/// Absolute, and baked in at compile time. A relative default resolves against
/// the working directory, so running from anywhere but the repository root would
/// serve nothing and answer every request with a 404: a server that looks
/// healthy and is not.
pub const DEFAULT_STATIC_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/static");

/// Where this example starts before the command line has its say.
pub fn defaults() -> Options {
  Options {
    static_dir: Some(DEFAULT_STATIC_DIR.to_owned()),
    ..Options::default()
  }
}

/// Parses argv, or returns a message to print and exit on.
pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Options, String> {
  playground_common::parse(args, defaults())
}

/// Rejects a role this build cannot perform, naming the feature that is missing.
///
/// The `cfg!`s are here rather than in the library because feature flags are per
/// crate: a check written inside `playground_common` would read `playground_common`'s
/// features and cheerfully approve a role this binary has no code for.
pub fn check_supported(role: Role) -> Result<(), String> {
  playground_common::check_supported(
    role,
    Support {
      server: cfg!(feature = "server"),
      websocket: cfg!(feature = "websocket"),
      client: cfg!(feature = "client"),
    },
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  fn args(rest: &[&str]) -> Vec<String> {
    std::iter::once("horde".to_owned()).chain(rest.iter().map(|s| (*s).to_owned())).collect()
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
  fn the_page_is_served_from_an_absolute_path_by_default() {
    // The whole class of "server runs, everything 404s" bug: a relative default
    // resolves against the working directory, so it works from the repository
    // root and silently serves nothing from anywhere else.
    let dir = parse(args(&[])).unwrap().static_dir.expect("a page by default");
    assert!(std::path::Path::new(&dir).is_absolute(), "{dir} must not depend on the working directory");
    assert!(dir.ends_with("/static"));
  }

  #[test]
  fn this_build_can_perform_the_role_it_defaults_to() {
    // A build that defaults to a role it has no code for fails its own check at
    // startup, which on wasm is a trap rather than a message.
    assert!(check_supported(Role::default()).is_ok());
  }
}
