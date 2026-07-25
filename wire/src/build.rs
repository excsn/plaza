//! Deriving a wire format's version from the source that defines it, for use
//! from a `build.rs`.
//!
//! # The problem this solves
//!
//! A browser client is a build product. It does not rebuild when the server
//! does, so a page built against an older wire format is the normal state of
//! affairs rather than an exotic one, and it fails in the least obvious way
//! available: the page loads, the game appears to run, and only the messages
//! whose shape changed are rejected. That reads as a netcode bug for as long as
//! it takes somebody to suspect the cache, which is a while.
//!
//! A version number in a handshake fixes it, and a version number maintained by
//! hand does not, because bumping it is exactly the step that gets skipped
//! during the change that needed it. Hashing the files that define the messages
//! makes the number a property of the code: two separate builds of the same
//! crate agree, and a stale bundle does not.
//!
//! # Using it
//!
//! Add this crate as a build dependency and call [`emit`]:
//!
//! ```toml
//! [build-dependencies]
//! plaza_wire = { version = "0.1", default-features = false, features = ["build"] }
//! ```
//!
//! ```no_run
//! // build.rs
//! fn main() {
//!   plaza_wire::build::emit(&["src/protocol.rs", "src/types.rs"]);
//! }
//! ```
//!
//! Then read it back in the crate itself:
//!
//! ```ignore
//! include!(concat!(env!("OUT_DIR"), "/wire_protocol.rs"));
//!
//! /// What this build speaks. Sent in the first message; a peer that disagrees
//! /// is told to reload rather than left half working.
//! pub const PROTOCOL: u32 = WIRE_PROTOCOL;
//! ```
//!
//! # Two limits worth knowing
//!
//! It cannot rescue a client older than the handshake itself, which is the
//! bootstrapping floor every protocol version has.
//!
//! And it hashes whole files rather than the type definitions alone, so editing
//! a *comment* in one of them also changes the version. That is a false
//! positive, and a deliberate one: the cost is telling a peer to reload when it
//! did not strictly need to, which is a page load, while the opposite mistake is
//! the silent half-working session the whole mechanism exists to prevent. Narrow
//! the file list rather than the hash if it becomes a nuisance.

use std::path::Path;

/// The FNV-1a offset basis, and where a fresh hash starts.
const BASIS: u32 = 2_166_136_261;
const PRIME: u32 = 16_777_619;

/// FNV-1a, written out rather than pulled in.
///
/// `DefaultHasher` is explicitly documented as not guaranteed stable across
/// releases, and this number has to mean the same thing in two separate
/// compilations, possibly by two different toolchains. A hash whose value is an
/// implementation detail is the one thing that cannot be used here.
fn fnv1a(bytes: &[u8], mut hash: u32) -> u32 {
  for byte in bytes {
    hash ^= *byte as u32;
    hash = hash.wrapping_mul(PRIME);
  }
  hash
}

/// Hashes some already-read source text into a version.
///
/// Carriage returns are stripped, so a checkout with CRLF line endings agrees
/// with one without. Order matters: the same files hashed in a different order
/// give a different version, which is harmless as long as one crate is
/// consistent with itself, and it is, because the list is written once.
///
/// Zero is never returned. It is reserved for "unknown", so a peer that could
/// not compute a version is never mistaken for one that agrees.
pub fn version_of_sources<I, B>(sources: I) -> u32
where
  I: IntoIterator<Item = B>,
  B: AsRef<[u8]>,
{
  let mut hash = BASIS;
  for source in sources {
    let text: Vec<u8> = source.as_ref().iter().copied().filter(|b| *b != b'\r').collect();
    hash = fnv1a(&text, hash);
  }
  hash.max(1)
}

/// Reads the given files and hashes them into a version.
///
/// Paths are relative to the crate root, the directory a build script runs in.
/// A missing file panics rather than being skipped: a version silently computed
/// over fewer files than intended would still look like a working version, and
/// would agree with builds it should not agree with, which is the exact failure
/// this is meant to catch.
pub fn version_of<P: AsRef<Path>>(sources: &[P]) -> u32 {
  let contents: Vec<Vec<u8>> = sources
    .iter()
    .map(|source| {
      let path = source.as_ref();
      std::fs::read(path).unwrap_or_else(|e| panic!("wire version: cannot read {}: {e}", path.display()))
    })
    .collect();
  version_of_sources(contents)
}

/// The whole build-script side: watch the sources, hash them, and publish the
/// result.
///
/// Publishes it two ways, so a crate can use whichever suits it:
///
/// - `$OUT_DIR/wire_protocol.rs`, defining `pub const WIRE_PROTOCOL: u32`, meant
///   to be `include!`d. Preferred, because it is already a number and needs no
///   parsing to reach a `const`.
/// - `cargo:rustc-env=WIRE_PROTOCOL`, for a crate that would rather read it with
///   `env!` and parse it itself.
///
/// Also emits `cargo:rerun-if-changed` for each source, so the version tracks
/// edits without a clean build.
pub fn emit<P: AsRef<Path>>(sources: &[P]) {
  for source in sources {
    println!("cargo:rerun-if-changed={}", source.as_ref().display());
  }
  let version = version_of(sources);

  println!("cargo:rustc-env=WIRE_PROTOCOL={version}");

  let out_dir = std::env::var("OUT_DIR").expect("wire version: OUT_DIR is unset, so this is not running as a build script");
  let path = Path::new(&out_dir).join("wire_protocol.rs");
  let generated = format!(
    "// Generated by plaza_wire::build. Do not edit; it is rewritten every build.\n\
     /// The wire format's version, hashed from the sources that define it.\n\
     pub const WIRE_PROTOCOL: u32 = {version};\n"
  );
  std::fs::write(&path, generated).unwrap_or_else(|e| panic!("wire version: cannot write {}: {e}", path.display()));
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_same_sources_always_give_the_same_version() {
    // The entire point: two separate compilations of the same code have to agree,
    // or the handshake tells everybody to reload forever.
    let sources = [&b"enum Op { Ping }"[..], &b"struct Packet;"[..]];
    assert_eq!(version_of_sources(sources), version_of_sources(sources));
  }

  #[test]
  fn changing_a_source_changes_the_version() {
    let before = version_of_sources([&b"enum Op { Ping }"[..]]);
    let after = version_of_sources([&b"enum Op { Ping, Pong }"[..]]);
    assert_ne!(before, after, "a wire change that did not move the version is the bug this prevents");
  }

  #[test]
  fn line_endings_do_not_change_the_version() {
    // Otherwise a checkout on Windows disagrees with one on Linux and every
    // cross-platform client is told it is outdated.
    let unix = version_of_sources([&b"enum Op {\n  Ping,\n}\n"[..]]);
    let dos = version_of_sources([&b"enum Op {\r\n  Ping,\r\n}\r\n"[..]]);
    assert_eq!(unix, dos);
  }

  #[test]
  fn the_version_is_never_zero() {
    // Zero means "unknown", so a peer that could not compute one is never
    // mistaken for a peer that agrees.
    assert_ne!(version_of_sources(Vec::<&[u8]>::new()), 0);
    assert_ne!(version_of_sources([&b""[..]]), 0);
  }

  #[test]
  fn every_source_in_the_list_counts() {
    // A version computed over fewer files than intended still looks like a
    // working version, and agrees with builds it should not agree with.
    let one = version_of_sources([&b"struct A;"[..]]);
    let both = version_of_sources([&b"struct A;"[..], &b"struct B;"[..]]);
    assert_ne!(one, both);
  }
}
