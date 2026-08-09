// The pure hashing core, dependency-free on purpose: plaza_wire's own build.rs
// includes this file textually to bake VOCAB_VERSION before the crate itself
// exists, which is also why these are plain comments; an include!d file cannot
// carry inner doc attributes.

/// The FNV-1a offset basis, and where a fresh hash starts.
pub(crate) const BASIS: u32 = 2_166_136_261;
const PRIME: u32 = 16_777_619;

/// FNV-1a, written out rather than pulled in.
///
/// `DefaultHasher` is explicitly documented as not guaranteed stable across
/// releases, and this number has to mean the same thing in two separate
/// compilations, possibly by two different toolchains. A hash whose value is an
/// implementation detail is the one thing that cannot be used here.
pub(crate) fn fnv1a(bytes: &[u8], mut hash: u32) -> u32 {
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
/// letting you list fewer files. [`Wire`](super::Wire) resolves types and lifts
/// the limit.
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
pub(crate) fn push_normalised(out: &mut String, line: &str) {
  for word in line.split_whitespace() {
    out.push_str(word);
  }
}

/// Drops the trailing commas rustfmt adds when it breaks a type across lines.
pub(crate) fn strip_trailing_commas(text: &str) -> String {
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
