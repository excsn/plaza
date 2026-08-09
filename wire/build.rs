//! Bakes `VOCAB_VERSION`: the hash of this crate's own wire vocabulary, so
//! consumer builds cover plaza's payload shapes without naming plaza's files.
//!
//! The hashing core is shared with the library by textual inclusion, because
//! this script runs before the crate it belongs to exists.

include!("src/build/hash.rs");

/// Every file whose types a consumer's ops may embed. `frame.rs` stays out:
/// its bodies are session-level, never inside an op.
const VOCAB_SOURCES: &[&str] = &["src/envelope.rs", "src/flow_payloads.rs", "src/payloads.rs"];

fn main() {
  let mut contents = Vec::new();
  for source in VOCAB_SOURCES {
    println!("cargo:rerun-if-changed={source}");
    contents.push(std::fs::read(source).unwrap_or_else(|e| panic!("vocab version: cannot read {source}: {e}")));
  }
  let version = version_of_sources(contents);
  let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
  let path = std::path::Path::new(&out_dir).join("vocab_version.rs");
  std::fs::write(&path, format!("{version}u32\n")).unwrap_or_else(|e| panic!("vocab version: cannot write {}: {e}", path.display()));
}
