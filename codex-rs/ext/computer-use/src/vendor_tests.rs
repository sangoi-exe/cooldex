use std::fs;

use codex_utils_cargo_bin::find_resource;
use pretty_assertions::assert_eq;
use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;

use crate::PROVENANCE_REL_PATH;
use crate::VENDORED_ARTIFACTS;

#[test]
fn provenance_file_lists_every_declared_artifact() {
    let provenance_path =
        find_resource!("vendor/openai/PROVENANCE.json").expect("provenance should resolve");
    let provenance: Value =
        serde_json::from_slice(&fs::read(&provenance_path).expect("provenance should read"))
            .expect("provenance should parse");

    let listed_artifacts = provenance["artifacts"]
        .as_array()
        .expect("artifacts should be an array")
        .iter()
        .map(|artifact| {
            (
                artifact["relative_path"]
                    .as_str()
                    .expect("relative_path should be a string")
                    .to_string(),
                artifact["bytes"]
                    .as_u64()
                    .expect("bytes should be an integer"),
                artifact["sha256"]
                    .as_str()
                    .expect("sha256 should be a string")
                    .to_string(),
            )
        })
        .collect::<Vec<_>>();
    let declared_artifacts = VENDORED_ARTIFACTS
        .iter()
        .map(|artifact| {
            (
                artifact.relative_path.to_string(),
                artifact.bytes,
                artifact.sha256.to_string(),
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(listed_artifacts, declared_artifacts);
    assert_eq!(
        provenance["publication_status"]
            .as_str()
            .expect("publication_status should be a string"),
        "Local-only. Any remote release, push, or redistribution requires a future explicit user decision."
    );
}

#[test]
fn vendored_artifacts_match_declared_size_and_hash() {
    let provenance_path =
        find_resource!("vendor/openai/PROVENANCE.json").expect("provenance should resolve");
    let vendor_root = provenance_path
        .parent()
        .expect("provenance should have a parent directory");

    for artifact in VENDORED_ARTIFACTS {
        let artifact_path = vendor_root.join(artifact.relative_path);
        let bytes = fs::read(&artifact_path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", artifact_path.display()));
        let sha256 = format!("{:x}", Sha256::digest(&bytes));

        assert_eq!(bytes.len() as u64, artifact.bytes);
        assert_eq!(sha256, artifact.sha256);
    }

    assert!(vendor_root.join(PROVENANCE_REL_PATH).is_file());
}
