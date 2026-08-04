//! What this process is: one argument, four answers.
//!
//! The enum, the parsing and the feature check live in [`playground_common`],
//! because they are the same in every listen server. What is left here is only
//! what is this crate's own: the defaults it starts from, and the feature flags
//! it was built with, which a library cannot read for it.

pub use playground_common::{usage, Options, Role};

use playground_common::Support;

/// This crate's `static/`, as an absolute path.
///
/// Absolute, and baked in at compile time. A relative default resolves against
/// the working directory, so running from anywhere but the repository root
/// would serve nothing and answer every request with a 404.
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

/// Rejects a role this build cannot perform, naming the feature that is
/// missing.
///
/// The `cfg!`s are here rather than in the library because feature flags are
/// per crate: a check written inside `playground_common` would read
/// `playground_common`'s features and cheerfully approve a role this binary has
/// no code for.
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
