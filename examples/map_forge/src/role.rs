//! What this process is: one argument, four answers. The vocabulary lives in
//! [`playground_common`]; here is only this crate's defaults and the feature
//! flags it was built with.

pub use playground_common::{usage, Options, Role};

use playground_common::Support;

pub const DEFAULT_STATIC_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/static");

pub fn defaults() -> Options {
  Options {
    static_dir: Some(DEFAULT_STATIC_DIR.to_owned()),
    bind: "0.0.0.0:8099".to_owned(),
    connect: "ws://127.0.0.1:8099/ws".to_owned(),
    ..Options::default()
  }
}

pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Options, String> {
  playground_common::parse(args, defaults())
}

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
