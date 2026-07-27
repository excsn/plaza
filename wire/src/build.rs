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

/// Extracts the type definitions from Rust source, discarding everything else.
///
/// This is what makes the version mean "the wire shape changed" rather than
/// "the file changed". A server gets bug fixes, and a bug fix in a file that
/// also happens to define a message used to bump the version and tell every
/// client to reload. So do comments, formatting, and any helper function
/// sharing the file.
///
/// Kept: `struct`, `enum` and `union` definitions, with their attributes, so a
/// `#[serde(rename)]`, an explicit discriminant, a new field, a new variant, or
/// a reordering all move the hash. Discarded: comments, `use`, `impl`, `fn`,
/// `const`, and whitespace.
///
/// **The limit worth knowing.** This reads the text, it does not resolve types.
/// If a field's type is defined in a file you did not list, changing that type
/// does not move the version. List every file that defines part of your wire
/// format, exactly as before; the narrowing here is about noise, not about
/// letting you list fewer files.
pub fn type_definitions(source: &[u8]) -> String {
  let text = String::from_utf8_lossy(source);
  let mut kept = String::new();
  let mut pending_attrs = String::new();
  let mut depth = 0usize;
  let mut capturing = false;

  for raw_line in text.lines() {
    // Comments never affect the wire, and they are the biggest single source of
    // spurious version bumps.
    let line = match raw_line.find("//") {
      Some(at) => &raw_line[..at],
      None => raw_line,
    };
    let line = line.trim();
    if line.is_empty() && !capturing {
      continue;
    }

    if capturing {
      push_normalised(&mut kept, line);
      depth += line.matches('{').count();
      depth = depth.saturating_sub(line.matches('}').count());
      // A tuple or unit struct ends at a semicolon rather than a brace.
      if depth == 0 && (line.ends_with('}') || line.ends_with(';')) {
        capturing = false;
      }
      continue;
    }

    if line.starts_with("#[") || line.starts_with("#!") {
      push_normalised(&mut pending_attrs, line);
      continue;
    }

    if declares_type(line) {
      kept.push_str(&pending_attrs);
      pending_attrs.clear();
      push_normalised(&mut kept, line);
      depth = line.matches('{').count().saturating_sub(line.matches('}').count());
      capturing = depth > 0;
      continue;
    }

    // Anything else (a fn, an impl, a use) drops the attributes it had gathered.
    pending_attrs.clear();
  }
  kept
}

fn declares_type(line: &str) -> bool {
  let mut words = line.split_whitespace().peekable();
  while let Some(word) = words.next() {
    match word {
      "pub" => continue,
      w if w.starts_with("pub(") => continue,
      "struct" | "enum" | "union" => return words.peek().is_some(),
      _ => return false,
    }
  }
  false
}

/// Appends a line with every run of whitespace removed.
///
/// Whitespace never reaches the wire, so `enum Op { Ping }` and a reformatted
/// `enum Op {\n  Ping,\n}` have to hash the same. The trailing comma before a
/// closing brace goes too, since rustfmt adds and removes it freely.
fn push_normalised(out: &mut String, line: &str) {
  for word in line.split_whitespace() {
    out.push_str(word);
  }
}

/// Drops the trailing commas rustfmt adds when it breaks a type across lines.
fn strip_trailing_commas(text: &str) -> String {
  text.replace(",}", "}").replace(",)", ")")
}

/// Hashes some already-read source text into a version.
///
/// Only the type definitions count; see [`type_definitions`] for what that
/// means and what it does not catch.
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
    hash = fnv1a(strip_trailing_commas(&type_definitions(&text)).as_bytes(), hash);
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
