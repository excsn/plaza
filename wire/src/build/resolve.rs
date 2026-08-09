//! [`Wire`]: the version derived by resolving types instead of listing files.
//!
//! The file-list [`emit`](super::emit) reads text and cannot follow a type into
//! another file, so a payload defined elsewhere silently does not count, and
//! the person who forgets a file gets a version that lies. This resolver lifts
//! that: tag each op enum with a doc line, and everything else is derived.
//!
//! ```text
//! /// plaza-wire: root
//! #[derive(Clone, Debug, Serialize, Deserialize)]
//! pub enum TableOp { ... }
//! ```
//!
//! ```no_run
//! // build.rs
//! fn main() {
//!   plaza_wire::build::Wire::detect()
//!     .dart("../../flutter/my_client/lib/wire_protocol.dart")
//!     .emit();
//! }
//! ```
//!
//! The scanner parses every file under `src/`, starts from the tagged roots,
//! and walks field types transitively, generic arguments included. The version
//! hashes exactly the reachable definitions, so an unrelated type sharing a
//! file no longer moves it. Plaza's own vocabulary (the notice payloads, the
//! netcode payloads, `Agent`) is covered by [`VOCAB_VERSION`](super::VOCAB_VERSION),
//! baked into this crate, so it is never yours to list. A referenced type the
//! resolver cannot place **fails the build naming the reference**, which is the
//! point: the file-list mechanism's failure mode was silence.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use proc_macro2::LineColumn;
use syn::spanned::Spanned;

use super::hash::{fnv1a, push_normalised, strip_trailing_commas, type_definitions, BASIS};

/// The doc tag that marks an op enum as a wire root.
pub const ROOT_TAG: &str = "plaza-wire: root";
/// The doc tag that says a serde type is deliberately not on the wire,
/// silencing the untagged-root warning for it.
pub const OFF_WIRE_TAG: &str = "plaza-wire: off-wire";

/// Names whose serde form is stable without their definition being scanned.
const STD_LEAVES: &[&str] = &[
  "bool", "char", "f32", "f64", "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128",
  "usize", "str", "String", "Option", "Vec", "Box", "Arc", "Rc", "Cow", "HashMap", "BTreeMap", "HashSet", "BTreeSet",
  "VecDeque", "PhantomData", "Duration", "Uuid",
];

/// Plaza vocabulary covered by [`VOCAB_VERSION`](super::VOCAB_VERSION).
const VOCAB_LEAVES: &[&str] = &[
  "Agent",
  "SequencedClientInput",
  "AuthoritativeStateUpdate",
  "TimestampedClientAction",
  "RemoteEntitySnapshot",
  "PhaseChangedNoticePayload",
  "RequestPhaseTransitionPayload",
  "CountdownTickNoticePayload",
  "EndTurnRequestPayload",
  "TurnChangedNoticePayload",
  "RoundStartedNoticePayload",
  "RoundEndedNoticePayload",
];

/// The one-line fix for a plaza type that lives in core rather than here:
/// the bundle in [`super::vocab`] carrying its vendored definition.
pub(crate) fn bundle_hint(name: &str) -> Option<&'static str> {
  const MATH: &[&str] = &["Vec2", "Vec3", "Quat"];
  const APP_COMMON: &[&str] = &[
    "ActivityStatusPayload",
    "CreateObjectPayload",
    "CursorPositionPayload",
    "DeleteObjectPayload",
    "DeleteObjectPropertyPayload",
    "InsertListItemPayload",
    "LockAcquiredNoticePayload",
    "LockDeniedNoticePayload",
    "LockReleasedNoticePayload",
    "MoveListItemPayload",
    "PresenceChangedNoticePayload",
    "ReleaseLockPayload",
    "RemoveListItemPayload",
    "RequestLockPayload",
    "SelectionPayload",
    "SetObjectPropertyPayload",
    "UpdateListItemPayload",
    "UpdatePresencePayload",
  ];
  if MATH.contains(&name) {
    Some(".vocab(plaza_wire::build::vocab::MATH)")
  } else if APP_COMMON.contains(&name) {
    Some(".vocab(plaza_wire::build::vocab::APP_COMMON)")
  } else {
    None
  }
}

/// Derives the wire version by resolving types from tagged roots.
///
/// See the [module docs](self) for the tags and the walk. `emit()` publishes
/// the version exactly as [`super::emit`] does, plus the optional Dart const.
pub struct Wire {
  roots: Option<Vec<String>>,
  scan_dirs: Vec<PathBuf>,
  dart: Option<PathBuf>,
  dart_types: Option<PathBuf>,
  leaves: Vec<String>,
  vocab: Vec<(String, String)>,
}

impl Wire {
  /// Roots are the types tagged `/// plaza-wire: root` anywhere under `src/`.
  pub fn detect() -> Self {
    Self::with_roots(None)
  }

  /// Roots named explicitly, for a crate that would rather not tag.
  pub fn ops(roots: &[&str]) -> Self {
    Self::with_roots(Some(roots.iter().map(|s| s.to_string()).collect()))
  }

  fn with_roots(roots: Option<Vec<String>>) -> Self {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    Self {
      roots,
      scan_dirs: vec![Path::new(&manifest).join("src")],
      dart: None,
      dart_types: None,
      leaves: Vec::new(),
      vocab: Vec::new(),
    }
  }

  /// Includes a vocabulary bundle: extra definition sources resolved, covered
  /// by the version, and emitted by [`dart_types`](Self::dart_types) exactly
  /// like your own types. [`super::vocab`] ships plaza's bundles; anything
  /// else takes the same shape, `(label, source_text)` pairs, so a vendored
  /// copy of a third-party definition can be included the same way (pin your
  /// copy with a test, as `wire/tests/vocab_sync.rs` pins plaza's).
  ///
  /// Your own definition of a name shadows a bundle's.
  pub fn vocab(mut self, bundle: &[(&str, &str)]) -> Self {
    for (name, source) in bundle {
      self.vocab.push((name.to_string(), source.to_string()));
    }
    self
  }

  /// Scans another directory besides `src/`, for a workspace that keeps wire
  /// types in a sibling crate it owns.
  pub fn also_scan(mut self, dir: impl AsRef<Path>) -> Self {
    self.scan_dirs.push(dir.as_ref().to_path_buf());
    self
  }

  /// Also writes the version where a paired Dart client imports it. See
  /// [`emit_dart`](super::emit_dart) for the contract; the file is committed.
  pub fn dart(mut self, path: impl AsRef<Path>) -> Self {
    self.dart = Some(path.as_ref().to_path_buf());
    self
  }

  /// Also generates Dart types for the whole resolved wire, at `path`, meant
  /// to be committed beside the version const. The generated classes encode
  /// and decode the exact shapes serde produces, compact MessagePack arrays
  /// included, which is what makes the compact codec safe from Dart: field
  /// order is generated from the Rust definitions instead of remembered.
  ///
  /// `Uuid` maps to a Dart `String`, which is what serde writes under
  /// human-readable codecs (JSON, named MessagePack with string values); a
  /// binary serializer writes `Uuid` as 16 raw bytes, so keep `Uuid` off
  /// compact wires or carry it as an explicit `String` field.
  pub fn dart_types(mut self, path: impl AsRef<Path>) -> Self {
    self.dart_types = Some(path.as_ref().to_path_buf());
    self
  }

  /// Declares a name a leaf the resolver should not chase: a macro-generated
  /// type, or one whose wire shape is pinned elsewhere. **Uncovered by the
  /// version**, which is why this is an explicit acknowledgement and not a
  /// default.
  pub fn leaf(mut self, name: &str) -> Self {
    self.leaves.push(name.to_string());
    self
  }

  /// Resolves, hashes, and publishes: `$OUT_DIR/wire_protocol.rs`,
  /// `cargo:rustc-env=WIRE_PROTOCOL`, rerun directives for the scanned
  /// directories, and the Dart const if [`dart`](Self::dart) was given.
  pub fn emit(self) {
    for dir in &self.scan_dirs {
      println!("cargo:rerun-if-changed={}", dir.display());
    }
    let version = self.version();
    super::publish(version);
    if let Some(dart) = &self.dart {
      println!("cargo:rerun-if-changed={}", dart.display());
      super::write_dart(version, dart);
    }
    if let Some(path) = &self.dart_types {
      println!("cargo:rerun-if-changed={}", path.display());
      let generated = super::dart_types::generate(&self);
      super::write_if_changed(path, &generated);
    }
  }

  /// The scanned index and the resolved roots, for the Dart type emitter.
  pub(crate) fn scanned(&self) -> (BTreeMap<String, Definition>, Vec<String>) {
    let index = self.index();
    let roots = self.find_roots(&index);
    (index, roots)
  }

  /// The scanned directories plus any included vocabulary bundles, the user's
  /// own definitions shadowing a bundle's.
  fn index(&self) -> BTreeMap<String, Definition> {
    let mut index = scan(&self.scan_dirs);
    for (name, source) in &self.vocab {
      let parsed = syn::parse_file(source).unwrap_or_else(|e| panic!("plaza-wire: cannot parse vocab {name}: {e}"));
      let mut bundle = BTreeMap::new();
      index_items(&parsed.items, source, Path::new(name), &mut bundle);
      for (n, def) in bundle {
        index.entry(n).or_insert(def);
      }
    }
    index
  }

  /// The derived version alone, for tests and for placing it yourself.
  ///
  /// Panics with the full list of unresolved references, because this runs in a
  /// build script and a panic is a build error naming the problem.
  pub fn version(&self) -> u32 {
    let index = self.index();
    let roots = self.find_roots(&index);
    let mut included: BTreeMap<&str, &Definition> = BTreeMap::new();
    let mut errors: Vec<String> = Vec::new();
    let mut queue: Vec<&str> = roots.iter().map(String::as_str).collect();

    while let Some(name) = queue.pop() {
      let Some(def) = index.get(name) else { continue };
      if included.insert(name, def).is_some() {
        continue;
      }
      for opaque in &def.opaque {
        errors.push(format!(
          "`{name}` ({}) contains {opaque}, which has no defined wire shape",
          def.file.display()
        ));
      }
      for reference in &def.refs {
        let reference = reference.as_str();
        if STD_LEAVES.contains(&reference) || self.leaves.iter().any(|l| l == reference) {
          continue;
        }
        if VOCAB_LEAVES.contains(&reference) {
          continue;
        }
        if index.contains_key(reference) {
          queue.push(reference);
        } else if let Some(hint) = bundle_hint(reference) {
          println!(
            "cargo:warning=plaza-wire: `{reference}` (via `{name}`) is plaza vocabulary not included in the \
             version; add {hint} to cover it"
          );
        } else {
          errors.push(format!(
            "cannot resolve `{reference}`, referenced from `{name}` ({}). Define it in a scanned directory, add \
             .also_scan(dir), or acknowledge it with .leaf(\"{reference}\") if its wire shape is pinned elsewhere",
            def.file.display()
          ));
        }
      }
    }

    if !errors.is_empty() {
      panic!("plaza-wire: the wire does not resolve:\n  - {}", errors.join("\n  - "));
    }
    self.warn_untagged(&index, &included);

    let mut hash = BASIS;
    for (name, def) in &included {
      hash = fnv1a(name.as_bytes(), hash);
      hash = fnv1a(strip_trailing_commas(&def.hash_text).as_bytes(), hash);
    }
    hash = fnv1a(&super::VOCAB_VERSION.to_be_bytes(), hash);
    hash.max(1)
  }

  fn find_roots(&self, index: &BTreeMap<String, Definition>) -> Vec<String> {
    match &self.roots {
      Some(named) => {
        let missing: Vec<&String> = named.iter().filter(|name| !index.contains_key(*name)).collect();
        if !missing.is_empty() {
          panic!("plaza-wire: named roots not found in the scanned directories: {missing:?}");
        }
        named.clone()
      }
      None => {
        let tagged: Vec<String> = index
          .iter()
          .filter(|(_, def)| def.root_tagged)
          .map(|(name, _)| name.clone())
          .collect();
        if tagged.is_empty() {
          panic!(
            "plaza-wire: no roots found. Tag each op enum with a doc line reading `/// {ROOT_TAG}`, or name them \
             with Wire::ops(&[..])"
          );
        }
        tagged
      }
    }
  }

  /// A serde type nobody references and nothing reaches is either a forgotten
  /// root or not wire at all; only its author knows which, so this is a warning
  /// that names it and both tags.
  fn warn_untagged(&self, index: &BTreeMap<String, Definition>, included: &BTreeMap<&str, &Definition>) {
    if self.roots.is_some() {
      return;
    }
    let referenced: BTreeSet<&str> = index.values().flat_map(|def| def.refs.iter().map(String::as_str)).collect();
    for (name, def) in index {
      if def.serde_derived
        && !def.off_wire_tagged
        && !included.contains_key(name.as_str())
        && !referenced.contains(name.as_str())
      {
        println!(
          "cargo:warning=plaza-wire: `{name}` ({}) derives serde but is unreachable from every root; tag it \
           `/// {ROOT_TAG}` if clients see it, or `/// {OFF_WIRE_TAG}` to silence this",
          def.file.display()
        );
      }
    }
  }
}

/// Plaza's own vocabulary sources, embedded so the emitter can resolve and
/// monomorphise payload types wherever this crate was compiled from, the cargo
/// registry included.
const EMBEDDED_VOCAB: &[(&str, &str)] = &[
  ("<plaza_wire>/envelope.rs", include_str!("../envelope.rs")),
  ("<plaza_wire>/flow_payloads.rs", include_str!("../flow_payloads.rs")),
  ("<plaza_wire>/payloads.rs", include_str!("../payloads.rs")),
];

/// The vocabulary definitions, indexed like a scan.
pub(crate) fn vocab_index() -> BTreeMap<String, Definition> {
  let mut index = BTreeMap::new();
  for (name, source) in EMBEDDED_VOCAB {
    let parsed = syn::parse_file(source).unwrap_or_else(|e| panic!("plaza-wire: cannot parse embedded {name}: {e}"));
    index_items(&parsed.items, source, Path::new(name), &mut index);
  }
  index
}

pub(crate) struct Definition {
  /// Normalised definition text, the same pipeline the file hash uses.
  hash_text: String,
  refs: Vec<String>,
  opaque: Vec<String>,
  root_tagged: bool,
  off_wire_tagged: bool,
  serde_derived: bool,
  pub(crate) file: PathBuf,
  /// Retained for the Dart type emitter, which needs the full definition.
  pub(crate) item: syn::Item,
}

fn scan(dirs: &[PathBuf]) -> BTreeMap<String, Definition> {
  let mut files = Vec::new();
  for dir in dirs {
    collect_files(dir, &mut files);
  }
  files.sort();

  let mut index: BTreeMap<String, Definition> = BTreeMap::new();
  for file in files {
    let source = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("plaza-wire: cannot read {}: {e}", file.display()));
    let parsed = syn::parse_file(&source).unwrap_or_else(|e| panic!("plaza-wire: cannot parse {}: {e}", file.display()));
    index_items(&parsed.items, &source, &file, &mut index);
  }
  index
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
  let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("plaza-wire: cannot scan {}: {e}", dir.display()));
  for entry in entries {
    let path = entry.expect("dir entry").path();
    if path.is_dir() {
      collect_files(&path, out);
    } else if path.extension().is_some_and(|ext| ext == "rs") {
      out.push(path);
    }
  }
}

fn index_items(items: &[syn::Item], source: &str, file: &Path, index: &mut BTreeMap<String, Definition>) {
  for item in items {
    match item {
      syn::Item::Struct(s) => {
        let (refs, opaque) = struct_refs(s);
        insert(index, s.ident.to_string(), &s.attrs, item, refs, opaque, source, file);
      }
      syn::Item::Enum(e) => {
        let mut refs = Vec::new();
        let mut opaque = Vec::new();
        let exclude = generic_params(&e.generics);
        for variant in &e.variants {
          for field in &variant.fields {
            collect_type(&field.ty, &exclude, &mut refs, &mut opaque);
          }
        }
        insert(index, e.ident.to_string(), &e.attrs, item, refs, opaque, source, file);
      }
      syn::Item::Type(alias) => {
        let mut refs = Vec::new();
        let mut opaque = Vec::new();
        collect_type(&alias.ty, &generic_params(&alias.generics), &mut refs, &mut opaque);
        insert(index, alias.ident.to_string(), &alias.attrs, item, refs, opaque, source, file);
      }
      syn::Item::Mod(module) => {
        if let Some((_, items)) = &module.content {
          index_items(items, source, file, index);
        }
      }
      _ => {}
    }
  }
}

#[allow(clippy::too_many_arguments)]
fn insert(
  index: &mut BTreeMap<String, Definition>,
  name: String,
  attrs: &[syn::Attribute],
  item: &syn::Item,
  refs: Vec<String>,
  opaque: Vec<String>,
  source: &str,
  file: &Path,
) {
  let definition = Definition {
    hash_text: hash_text(item, source),
    item: item.clone(),
    refs,
    opaque,
    root_tagged: doc_contains(attrs, ROOT_TAG),
    off_wire_tagged: doc_contains(attrs, OFF_WIRE_TAG),
    serde_derived: derives_serde(attrs),
    file: file.to_path_buf(),
  };
  if let Some(previous) = index.insert(name.clone(), definition) {
    panic!(
      "plaza-wire: two definitions of `{name}` ({} and {}). The resolver indexes by bare name, so wire types need \
       unique names; rename one, or use Wire::ops with unambiguous roots and keep the duplicate off the wire",
      previous.file.display(),
      file.display()
    );
  }
}

/// The definition's own source text, sliced by span and run through the same
/// normalisation the file hash uses, so a doc edit or reformat moves nothing
/// and a field, attribute or variant change moves the version.
fn hash_text(item: &syn::Item, source: &str) -> String {
  let span = item.span();
  let sliced = slice(source, span.start(), span.end());
  match item {
    // `type_definitions` drops aliases (no struct/enum keyword), so normalise
    // an alias line directly.
    syn::Item::Type(_) => {
      let mut out = String::new();
      for line in sliced.lines() {
        let line = match line.find("//") {
          Some(at) => &line[..at],
          None => line,
        };
        push_normalised(&mut out, line.trim());
      }
      out
    }
    _ => type_definitions(sliced.as_bytes()),
  }
}

fn slice(source: &str, start: LineColumn, end: LineColumn) -> String {
  let lines: Vec<&str> = source.lines().collect();
  let mut out = String::new();
  for line_number in start.line..=end.line {
    let Some(line) = lines.get(line_number - 1) else { break };
    let chars: Vec<char> = line.chars().collect();
    let from = if line_number == start.line { start.column } else { 0 };
    let to = if line_number == end.line { end.column.min(chars.len()) } else { chars.len() };
    out.extend(chars.get(from..to).unwrap_or_default());
    out.push('\n');
  }
  out
}

fn struct_refs(s: &syn::ItemStruct) -> (Vec<String>, Vec<String>) {
  let mut refs = Vec::new();
  let mut opaque = Vec::new();
  let exclude = generic_params(&s.generics);
  for field in &s.fields {
    collect_type(&field.ty, &exclude, &mut refs, &mut opaque);
  }
  (refs, opaque)
}

fn generic_params(generics: &syn::Generics) -> BTreeSet<String> {
  generics
    .type_params()
    .map(|param| param.ident.to_string())
    .collect()
}

fn collect_type(ty: &syn::Type, exclude: &BTreeSet<String>, refs: &mut Vec<String>, opaque: &mut Vec<String>) {
  match ty {
    syn::Type::Path(path) => {
      if let Some(segment) = path.path.segments.last() {
        let name = segment.ident.to_string();
        if !exclude.contains(&name) {
          refs.push(name);
        }
        if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
          for arg in &args.args {
            if let syn::GenericArgument::Type(inner) = arg {
              collect_type(inner, exclude, refs, opaque);
            }
          }
        }
      }
    }
    syn::Type::Tuple(tuple) => {
      for elem in &tuple.elems {
        collect_type(elem, exclude, refs, opaque);
      }
    }
    syn::Type::Array(array) => collect_type(&array.elem, exclude, refs, opaque),
    syn::Type::Slice(slice) => collect_type(&slice.elem, exclude, refs, opaque),
    syn::Type::Paren(paren) => collect_type(&paren.elem, exclude, refs, opaque),
    syn::Type::Group(group) => collect_type(&group.elem, exclude, refs, opaque),
    syn::Type::Reference(reference) => collect_type(&reference.elem, exclude, refs, opaque),
    other => opaque.push(format!("`{}`", quote_type(other))),
  }
}

fn quote_type(ty: &syn::Type) -> String {
  match ty {
    syn::Type::TraitObject(_) => "a trait object".into(),
    syn::Type::ImplTrait(_) => "an impl-trait type".into(),
    syn::Type::Macro(_) => "a macro-generated type".into(),
    syn::Type::BareFn(_) => "a function pointer".into(),
    _ => "an unsupported type form".into(),
  }
}

fn doc_contains(attrs: &[syn::Attribute], tag: &str) -> bool {
  attrs.iter().any(|attr| {
    if !attr.path().is_ident("doc") {
      return false;
    }
    if let syn::Meta::NameValue(nv) = &attr.meta
      && let syn::Expr::Lit(lit) = &nv.value
      && let syn::Lit::Str(text) = &lit.lit
    {
      return text.value().trim() == tag;
    }
    false
  })
}

fn derives_serde(attrs: &[syn::Attribute]) -> bool {
  attrs.iter().any(|attr| {
    if !attr.path().is_ident("derive") {
      return false;
    }
    let mut found = false;
    let _ = attr.parse_nested_meta(|meta| {
      if meta.path.segments.last().is_some_and(|s| s.ident == "Serialize" || s.ident == "Deserialize") {
        found = true;
      }
      Ok(())
    });
    found
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn crate_dir(test: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("plaza_wire_resolve_{}_{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (name, content) in files {
      let path = dir.join(name);
      std::fs::create_dir_all(path.parent().unwrap()).unwrap();
      std::fs::write(path, content).unwrap();
    }
    dir
  }

  fn wire_over(dir: &Path) -> Wire {
    Wire {
      roots: None,
      scan_dirs: vec![dir.to_path_buf()],
      dart: None,
      dart_types: None,
      leaves: Vec::new(),
      vocab: Vec::new(),
    }
  }

  #[test]
  fn the_walk_reaches_across_files_and_through_generics() {
    let dir = crate_dir("the_walk_reaches_across_files_and_through_generics", &[
      (
        "ops.rs",
        "/// plaza-wire: root\n#[derive(Serialize)]\npub enum Op { Play(Card), View(Box<Snapshot>) }\n",
      ),
      ("view.rs", "pub struct Snapshot { pub cards: Vec<(u64, Card)>, pub phase: Phase }\npub struct Card(pub u8);\n"),
      ("phase.rs", "pub enum Phase { Day, Night }\npub struct NotOnTheWire { pub secret: String }\n"),
    ]);
    let with_all = wire_over(&dir).version();

    // A reachable definition moves the version.
    std::fs::write(dir.join("phase.rs"), "pub enum Phase { Day, Night, Dusk }\npub struct NotOnTheWire { pub secret: String }\n").unwrap();
    let phase_changed = wire_over(&dir).version();
    assert_ne!(with_all, phase_changed, "a new variant two hops from the root");

    // An unreachable one does not, which the file hash could never say.
    std::fs::write(dir.join("phase.rs"), "pub enum Phase { Day, Night, Dusk }\npub struct NotOnTheWire { pub secret: u64 }\n").unwrap();
    assert_eq!(phase_changed, wire_over(&dir).version(), "an off-wire neighbour changed shape");
    std::fs::remove_dir_all(&dir).unwrap();
  }

  #[test]
  fn doc_edits_and_reformatting_move_nothing() {
    let dir = crate_dir("doc_edits_and_reformatting_move_nothing", &[("ops.rs", "/// plaza-wire: root\npub enum Op { Ping }\n")]);
    let plain = wire_over(&dir).version();
    std::fs::write(
      dir.join("ops.rs"),
      "/// plaza-wire: root\n///\n/// Now with prose.\npub enum Op {\n  Ping,\n}\n",
    )
    .unwrap();
    assert_eq!(plain, wire_over(&dir).version());
    std::fs::remove_dir_all(&dir).unwrap();
  }

  #[test]
  fn an_alias_is_followed_and_its_shape_counts() {
    let dir = crate_dir("an_alias_is_followed_and_its_shape_counts", &[(
      "ops.rs",
      "/// plaza-wire: root\npub enum Op { Join { room: RoomId } }\npub type RoomId = u64;\n",
    )]);
    let as_u64 = wire_over(&dir).version();
    std::fs::write(
      dir.join("ops.rs"),
      "/// plaza-wire: root\npub enum Op { Join { room: RoomId } }\npub type RoomId = String;\n",
    )
    .unwrap();
    assert_ne!(as_u64, wire_over(&dir).version(), "the alias target is the wire shape");
    std::fs::remove_dir_all(&dir).unwrap();
  }

  #[test]
  fn an_unresolved_reference_fails_the_build_by_name() {
    let dir = crate_dir("an_unresolved_reference_fails_the_build_by_name", &[("ops.rs", "/// plaza-wire: root\npub enum Op { Cast(Spell) }\n")]);
    let err = std::panic::catch_unwind(|| wire_over(&dir).version()).unwrap_err();
    let message = err.downcast_ref::<String>().unwrap();
    assert!(message.contains("`Spell`"), "{message}");
    assert!(message.contains("`Op`"), "names the referencing definition: {message}");
    assert_eq!(wire_over(&dir).leaf("Spell").version() > 0, true, "the acknowledged escape hatch");
    std::fs::remove_dir_all(&dir).unwrap();
  }

  #[test]
  fn plaza_vocabulary_is_a_leaf_and_still_versioned() {
    let dir = crate_dir("plaza_vocabulary_is_a_leaf_and_still_versioned", &[(
      "ops.rs",
      "/// plaza-wire: root\npub enum Op { Phase(PhaseChangedNoticePayload<Phase>) }\npub enum Phase { Day }\n",
    )]);
    // Resolves without listing plaza's files; the payload's shape rides in
    // through VOCAB_VERSION, mixed into every derived number.
    assert!(wire_over(&dir).version() > 0);
    std::fs::remove_dir_all(&dir).unwrap();
  }

  #[test]
  fn no_tagged_root_is_an_error_naming_the_tag() {
    let dir = crate_dir("no_tagged_root_is_an_error_naming_the_tag", &[("ops.rs", "pub enum Op { Ping }\n")]);
    let err = std::panic::catch_unwind(|| wire_over(&dir).version()).unwrap_err();
    assert!(err.downcast_ref::<String>().unwrap().contains(ROOT_TAG));
    std::fs::remove_dir_all(&dir).unwrap();
  }

  #[test]
  fn an_included_bundle_covers_and_the_leaf_does_not() {
    let dir = crate_dir("an_included_bundle_covers", &[(
      "ops.rs",
      "/// plaza-wire: root\npub enum Op { Move { to: Vec2 } }\n",
    )]);
    let mut with_bundle = wire_over(&dir);
    with_bundle.vocab = super::super::vocab::MATH.iter().map(|(n, s)| (n.to_string(), s.to_string())).collect();
    let covered = with_bundle.version();

    let acknowledged = wire_over(&dir).leaf("Vec2").version();
    assert_ne!(covered, acknowledged, "a covered definition is hashed; an acknowledged leaf is not");
    std::fs::remove_dir_all(&dir).unwrap();
  }

  #[test]
  fn named_roots_work_without_tags() {
    let dir = crate_dir("named_roots_work_without_tags", &[("ops.rs", "pub enum Op { Ping }\n")]);
    let wire = Wire {
      roots: Some(vec!["Op".into()]),
      scan_dirs: vec![dir.clone()],
      dart: None,
      dart_types: None,
      leaves: Vec::new(),
      vocab: Vec::new(),
    };
    assert!(wire.version() > 0);
    std::fs::remove_dir_all(&dir).unwrap();
  }
}
