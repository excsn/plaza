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
//! here. That vocabulary is needed by the browser client too, and a wasm bundle
//! must not inherit an HTTP server and an async runtime to learn the name of its
//! own role. The examples keep it in a dependency-free crate of their own; the
//! parsing around it is an opinion that any real application will already have.
//!
//! # One port
//!
//! It matters more than it sounds. A joiner is given a single URL, the page and
//! the socket come from the same origin, so there is no CORS story and no second
//! thing to configure. It is also what makes hosting a thing you can tell a
//! friend over a chat message.
//!
//! # Why the cache busting is not optional
//!
//! A browser client is a build product. It does not rebuild when the server
//! does, so a browser holding a bundle from before a wire change is the normal
//! state of affairs, and it fails in the least obvious way available: the page
//! loads, the application runs, and only the messages whose shape changed are
//! rejected. That reads as a protocol bug for as long as it takes somebody to
//! suspect the cache.
//!
//! [`Host::cache_bust`] answers it by stamping the asset's URL with its own
//! modification time, read per request rather than at startup, so rebuilding the
//! client reaches an already-running host without restarting it. A stamped URL
//! rather than cache headers alone, because a stamp is the only part of this
//! that survives an intermediary with its own opinions, and a deployed host sits
//! behind exactly that. The headers still matter, but on the *referencing* page:
//! a cached index would keep quoting the old stamp, which is the trap that makes
//! cache busting look like it does not work.
//!
//! Pair it with a protocol version derived at build time (see
//! `plaza_wire::build`) so that a client which slips through anyway is told to
//! reload rather than left half working.

#[cfg(feature = "actix_host")]
mod server;
#[cfg(feature = "actix_host")]
pub use server::{init_logging, lan_address, Host};
