//! Standing a Plaza application up as a **listen server**: one process that is
//! the authority, optionally plays, and serves its own browser client from the
//! same port.
//!
//! [`Host`] is the HTTP side of that: it binds a port, serves a directory, and
//! carries the cache busting that a browser client which is also a build product
//! turns out to need. The application registers its own routes, which is where
//! the WebSocket goes, so none of this needs to know anything about the state
//! being shared.
//!
//! What a process *is* (headless, observer, host, joiner) is deliberately not
//! here. The browser client needs that vocabulary too and a wasm bundle must
//! not inherit an HTTP server and an async runtime to learn the name of its own
//! role. The examples keep it in a dependency-free crate of their own; any real
//! application already parses its own arguments.
//!
//! # One port
//!
//! A joiner is given a single URL that can be sent in a chat message. The page
//! and the socket come from the same origin, so there is no CORS setup and
//! nothing else to configure.
//!
//! # Cache busting
//!
//! A browser client is a build product. It does not rebuild when the server
//! does, so browsers holding a bundle from before a wire change are normal. The
//! page loads and the application runs, but the messages whose shape changed
//! are rejected. It looks like a protocol bug until somebody suspects the cache.
//!
//! [`Host::cache_bust`] stamps the asset's URL with its own modification time,
//! read per request rather than at startup, so rebuilding the client reaches an
//! already-running host without restarting it. It uses a stamped URL rather
//! than cache headers alone because a deployed host usually sits behind an
//! intermediary that applies its own caching policy. Only a changed URL gets
//! past it. The headers still matter on the *referencing* page: a cached
//! index keeps quoting the old stamp and cache busting then appears not to work.
//!
//! Pair it with a protocol version derived at build time (see
//! `plaza_wire::build`) so that a client which slips through anyway is told to
//! reload rather than left half working.

#[cfg(feature = "actix_host")]
mod server;
#[cfg(feature = "actix_host")]
mod sim;
#[cfg(feature = "actix_host")]
pub use server::{init_logging, lan_address, Host};
#[cfg(feature = "actix_host")]
pub use sim::{SimHost, SimWiring, DEFAULT_COMMAND_BUFFER, DEFAULT_WAKE_HZ};
