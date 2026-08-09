//! The vendored vocabulary copies are byte-identical to core's originals.
//!
//! `build::vocab` embeds copies because a published `plaza_wire` cannot read
//! core's files; this is the check that keeps a copy from drifting. Repo-only,
//! like `dart_fixtures`: it reads outside the package.

const PAIRS: &[(&str, &str)] = &[
  ("src/build/vocab/math.rs", "../core/src/common/math.rs"),
  ("src/build/vocab/app_common_locking.rs", "../core/src/app_common/locking/op_payloads.rs"),
  (
    "src/build/vocab/app_common_object_property_ops.rs",
    "../core/src/app_common/object_property_ops/op_payloads.rs",
  ),
  (
    "src/build/vocab/app_common_ordered_collection_ops.rs",
    "../core/src/app_common/ordered_collection_ops/op_payloads.rs",
  ),
  ("src/build/vocab/app_common_presence.rs", "../core/src/app_common/presence/op_payloads.rs"),
  (
    "src/build/vocab/app_common_presence_fragments.rs",
    "../core/src/app_common/presence/payload_fragments.rs",
  ),
];

#[test]
fn every_vendored_copy_matches_its_core_original() {
  for (copy, original) in PAIRS {
    let copy_path = format!("{}/{copy}", env!("CARGO_MANIFEST_DIR"));
    let original_path = format!("{}/{original}", env!("CARGO_MANIFEST_DIR"));
    let copied = std::fs::read(&copy_path).unwrap_or_else(|e| panic!("cannot read {copy_path}: {e}"));
    let source = std::fs::read(&original_path).unwrap_or_else(|e| panic!("cannot read {original_path}: {e}"));
    assert_eq!(
      copied, source,
      "{copy} drifted from {original}; re-copy it so included bundles keep matching what core compiles"
    );
  }
}
