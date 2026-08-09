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
//! And it hashes the **type definitions** in those files, not the files
//! themselves. That distinction is the difference between a version that means
//! something and one nobody can act on: a server gets bug fixes, and hashing
//! whole files meant every fix bumped the version and told every client to
//! reload, whether or not a message had changed shape. Comments, formatting,
//! `use`, `impl` and `fn` are all discarded; a field, a variant, an explicit
//! discriminant, a `#[serde]` attribute or a reordering all move it.
//!
//! It reads text and does not resolve types, so a field whose type is defined
//! in a file you did not list can change without moving the version. List every
//! file that defines part of your wire format. The narrowing is about noise,
//! not about listing fewer files.

use std::path::Path;

mod dart_types;
mod hash;
mod resolve;

pub use hash::{type_definitions, version_of_sources};
pub use resolve::Wire;

/// The version contribution of plaza's own wire vocabulary: [`Agent`], the
/// netcode payloads, and the flow-control notice payloads, hashed from this
/// crate's own sources when this crate was built.
///
/// [`Wire`] mixes it into every derived version automatically, so an
/// application whose ops embed a plaza payload never lists plaza's files and is
/// still covered: a payload shape change here moves every consumer's version on
/// their next `cargo update`.
///
/// [`Agent`]: crate::envelope::Agent
pub const VOCAB_VERSION: u32 = include!(concat!(env!("OUT_DIR"), "/vocab_version.rs"));

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
  publish(version_of(sources));
}

/// Publishes an already-derived version the two ways [`emit`] documents.
pub(crate) fn publish(version: u32) {
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

/// The Dart half of [`emit`]: writes the same derived version where a paired
/// Dart client imports it, so the handshake is computed on both ends of the
/// wire instead of computed on one and declared `unknown` on the other.
///
/// ```no_run
/// // build.rs of the server crate whose sources define the wire
/// fn main() {
///   let sources = ["src/types.rs"];
///   plaza_wire::build::emit(&sources);
///   plaza_wire::build::emit_dart(&sources, "../../flutter/my_client/lib/wire_protocol.dart");
/// }
/// ```
///
/// The generated file declares `const int wireProtocol` and is meant to be
/// **committed**: a Dart build cannot run this build script, so the committed
/// copy is what the Dart toolchain sees, and this function keeps it current.
/// The write is skipped when the content already matches, so an untouched wire
/// leaves the file untouched; a hand edit is healed on the next build, because
/// the file itself is watched. A missing parent directory panics rather than
/// being skipped, for [`version_of`]'s reason: a version silently not delivered
/// still looks like a working version.
///
/// Pin the committed copy from the server's own tests with
/// [`assert_dart_protocol`], so drift fails CI even when nothing rebuilds.
pub fn emit_dart<P: AsRef<Path>>(sources: &[P], dart_path: impl AsRef<Path>) {
  for source in sources {
    println!("cargo:rerun-if-changed={}", source.as_ref().display());
  }
  let dart_path = dart_path.as_ref();
  println!("cargo:rerun-if-changed={}", dart_path.display());
  write_dart(version_of(sources), dart_path);
}

pub(crate) fn write_dart(version: u32, dart_path: &Path) {
  write_if_changed(dart_path, &dart_version_file(version));
}

/// Writes only when the content differs, so an unchanged wire leaves the
/// committed file's mtime alone.
pub(crate) fn write_if_changed(path: &Path, content: &str) {
  match std::fs::read(path) {
    Ok(existing) if existing == content.as_bytes() => {}
    _ => {
      std::fs::write(path, content).unwrap_or_else(|e| panic!("wire version: cannot write {}: {e}", path.display()));
    }
  }
}

/// Asserts a committed `wire_protocol.dart` carries `expected`, for the pin
/// test beside a server whose build writes it:
///
/// ```ignore
/// #[test]
/// fn dart_protocol_is_current() {
///   plaza_wire::build::assert_dart_protocol("../../flutter/my_client/lib/wire_protocol.dart", PROTOCOL);
/// }
/// ```
///
/// The build script rewrites the file whenever the wire changes, so this can
/// only fail when a wire change was committed without building the server,
/// which is exactly the drift the handshake exists to catch. It is
/// defence-in-depth, not the safety net: a stale client also self-announces at
/// runtime through the `Hello` handshake.
pub fn assert_dart_protocol(dart_path: impl AsRef<Path>, expected: u32) {
  let path = dart_path.as_ref();
  let dart = std::fs::read_to_string(path)
    .unwrap_or_else(|e| panic!("the generated {} should be committed: {e}", path.display()));
  let declared = dart
    .lines()
    .find_map(|line| line.strip_prefix("const int wireProtocol = ")?.strip_suffix(';')?.trim().parse::<u32>().ok())
    .unwrap_or_else(|| panic!("{} carries no `const int wireProtocol = N;` line", path.display()));
  assert_eq!(
    declared, expected,
    "the Dart client would announce protocol {declared} where this build speaks {expected}; rebuild the server \
     crate to refresh {}",
    path.display()
  );
}

fn dart_version_file(version: u32) -> String {
  format!(
    "// Generated by plaza_wire::build from the paired server's wire sources.\n\
     // Do not edit; the server's build script rewrites it when the wire changes.\n\n\
     /// The wire format's version, hashed from the Rust sources that define it.\n\
     const int wireProtocol = {version};\n"
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_dart_file_carries_the_same_version_and_is_rewritten_only_on_change() {
    let dir = std::env::temp_dir().join(format!("plaza_wire_emit_dart_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("types.rs");
    let dart = dir.join("wire_protocol.dart");
    std::fs::write(&source, b"enum Op { Ping }").unwrap();

    emit_dart(&[&source], &dart);
    let written = std::fs::read_to_string(&dart).unwrap();
    assert!(
      written.contains(&format!("const int wireProtocol = {};", version_of(&[&source]))),
      "the Dart const is the same number the Rust const gets: {written}"
    );

    let before = std::fs::metadata(&dart).unwrap().modified().unwrap();
    emit_dart(&[&source], &dart);
    assert_eq!(
      std::fs::metadata(&dart).unwrap().modified().unwrap(),
      before,
      "an unchanged wire leaves the committed file untouched"
    );

    std::fs::write(&source, b"enum Op { Ping, Pong }").unwrap();
    emit_dart(&[&source], &dart);
    assert!(
      std::fs::read_to_string(&dart).unwrap().contains(&format!("= {};", version_of(&[&source]))),
      "a shape change rewrites it"
    );
    std::fs::remove_dir_all(&dir).unwrap();
  }

  #[test]
  fn the_same_sources_always_give_the_same_version() {
    // The entire point: two separate compilations of the same code have to agree,
    // or the handshake tells everybody to reload forever.
    let sources = [&b"enum Op { Ping }"[..], &b"struct Packet;"[..]];
    assert_eq!(version_of_sources(sources), version_of_sources(sources));
  }

  #[test]
  fn a_shape_change_moves_the_version() {
    let before = version_of_sources([&b"enum Op { Ping }"[..]]);
    assert_ne!(before, version_of_sources([&b"enum Op { Ping, Pong }"[..]]), "a new variant");
    assert_ne!(before, version_of_sources([&b"enum Op { Pong }"[..]]), "a renamed variant");
    let s = &b"struct P { a: u8, b: u8 }"[..];
    assert_ne!(
      version_of_sources([s]),
      version_of_sources([&b"struct P { b: u8, a: u8 }"[..]]),
      "reordering, which changes any positional encoding"
    );
    assert_ne!(
      version_of_sources([s]),
      version_of_sources([&b"struct P { a: u8, b: u16 }"[..]]),
      "a field's type"
    );
    assert_ne!(
      version_of_sources([&b"enum Op { A = 0 }"[..]]),
      version_of_sources([&b"enum Op { A = 7 }"[..]]),
      "an explicit discriminant"
    );
    assert_ne!(
      version_of_sources([&b"struct P { #[serde(rename = \"a\")] alpha: u8 }"[..]]),
      version_of_sources([&b"struct P { #[serde(rename = \"z\")] alpha: u8 }"[..]]),
      "a serde attribute, which changes the wire without changing the type"
    );
  }

  #[test]
  fn a_bug_fix_does_not_move_the_version() {
    // The reason this hashes definitions rather than files. A server ships
    // fixes; hashing whole files told every client to reload on each one.
    let before = version_of_sources([&b"enum Op { Ping }\nfn apply(x: u8) -> u8 { x + 1 }\n"[..]]);
    let after = version_of_sources([&b"enum Op { Ping }\nfn apply(x: u8) -> u8 { x.saturating_add(1) }\n"[..]]);
    assert_eq!(before, after, "a fix to a function sharing the file");
  }

  #[test]
  fn comments_and_formatting_do_not_move_the_version() {
    let plain = version_of_sources([&b"enum Op { Ping }"[..]]);
    assert_eq!(plain, version_of_sources([&b"/// Docs.\nenum Op { Ping }"[..]]), "a doc comment");
    assert_eq!(plain, version_of_sources([&b"enum Op {\n  Ping,\n}"[..]].map(|s| s)), "reformatting");
    assert_eq!(plain, version_of_sources([&b"enum Op { Ping } // trailing"[..]]), "a trailing comment");
    assert_eq!(plain, version_of_sources([&b"use std::fmt;\nenum Op { Ping }"[..]]), "an added import");
  }

  #[test]
  fn impls_are_not_part_of_the_shape() {
    // Methods do not serialize, so adding one must not tell clients to reload.
    let before = version_of_sources([&b"struct P { a: u8 }"[..]]);
    let after = version_of_sources([&b"struct P { a: u8 }\nimpl P { fn a(&self) -> u8 { self.a } }"[..]]);
    assert_eq!(before, after);
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
