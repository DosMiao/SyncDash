//! Version-store behavior: preservation routes, reverse deltas, indexing and pruning.

use std::path::PathBuf;

use super::model::*;
use super::retention::*;
use super::writer::VersionWriter;
use super::*;
use crate::foundation::path::{to_native, RootRelativePath};
use crate::fs::local_root::LocalRoot;
use crate::model::chunk::RecipeStep;

fn test_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "syncdash-version-test-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn whole_entry(rel: &str) -> PreservedEntry {
    PreservedEntry {
        relative_path: RootRelativePath::try_from(rel).unwrap(),
        payload_kind: VersionPayloadKind::Whole,
        reason: "test".to_owned(),
        old_hash: blake3::hash(b"archived").to_hex().to_string(),
        old_size: 8,
        old_mtime_ms: 0,
        old_mode: None,
        new_hash: None,
        recipe: None,
    }
}

#[test]
fn rdelta_roundtrip_bytes() {
    let old = vec![7u8; crate::model::chunk::DELTA_MIN_SIZE as usize + 256 * 1024];
    let mut new = old.clone();
    for byte in new.iter_mut().take(3_004_096).skip(3_000_000) {
        *byte = 9;
    }
    new.extend_from_slice(&[1u8; 2048]);

    let new_chunks = crate::fs::chunk::chunk_bytes(&new);
    let mut replacement_chunk_locations: std::collections::HashMap<&str, (u64, u32)> =
        std::collections::HashMap::new();
    for chunk in &new_chunks {
        replacement_chunk_locations
            .entry(chunk.hash.as_str())
            .or_insert((chunk.off, chunk.len));
    }
    let mut blob: Vec<u8> = Vec::new();
    let mut recipe: Vec<RecipeStep> = Vec::new();
    for chunk in crate::fs::chunk::chunk_bytes(&old) {
        if let Some(&(replacement_offset, replacement_length)) =
            replacement_chunk_locations.get(chunk.hash.as_str())
        {
            recipe.push(RecipeStep {
                s: "base".into(),
                off: replacement_offset,
                len: replacement_length,
            });
        } else {
            let off = blob.len() as u64;
            blob.extend_from_slice(
                &old[chunk.off as usize..(chunk.off + chunk.len as u64) as usize],
            );
            recipe.push(RecipeStep {
                s: "blob".into(),
                off,
                len: chunk.len,
            });
        }
    }
    assert!(
        blob.len() < old.len() / 4,
        "rdelta blob should be much smaller (got {} of {})",
        blob.len(),
        old.len()
    );

    let mut rebuilt: Vec<u8> = Vec::new();
    for st in &recipe {
        let (off, len) = (st.off as usize, st.len as usize);
        let s = if st.s == "base" {
            &new[off..off + len]
        } else {
            &blob[off..off + len]
        };
        rebuilt.extend_from_slice(s);
    }
    assert_eq!(blake3::hash(&rebuilt), blake3::hash(&old));
}

#[test]
fn malformed_index_data_is_not_silently_ignored() {
    let root = test_root("bad-index");
    let store = root.join(crate::foundation::names::VERSION_STORE_DIR);
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("index.jsonl"), b"not-json\n").unwrap();
    let error = match list(&root) {
        Ok(_) => panic!("malformed index data must fail"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn typed_version_fields_keep_the_existing_disk_shape() {
    let manifest: VersionManifest = serde_json::from_str(
        r#"{"id":"v1","ts_ms":1,"host":"test","entries":[{"rel":"a.txt","kind":"whole","why":"deleted","old_hash":"","old_size":0,"old_mtime_ms":0}]}"#,
    )
    .unwrap();
    assert_eq!(manifest.id.as_str(), "v1");
    assert_eq!(manifest.entries[0].relative_path.as_str(), "a.txt");
    assert_eq!(manifest.entries[0].payload_kind, VersionPayloadKind::Whole);
    assert_eq!(manifest.entries[0].reason, "deleted");

    let encoded = serde_json::to_value(&manifest).unwrap();
    assert_eq!(encoded["id"], "v1");
    assert_eq!(encoded["entries"][0]["rel"], "a.txt");
    assert_eq!(encoded["entries"][0]["kind"], "whole");
    assert_eq!(encoded["entries"][0]["why"], "deleted");
}

#[test]
fn finish_never_deletes_an_unindexed_displaced_payload() {
    let root = test_root("unindexed-payload");
    let local_root = LocalRoot::open(root.clone()).unwrap();
    let mut writer = VersionWriter::begin(&local_root, || Ok(())).unwrap();
    let version_directory = root.join(to_native(writer.version_dir.as_str()));
    let recovery_file = version_directory.join("recover-me");
    std::fs::write(&recovery_file, b"original").unwrap();
    writer.has_unindexed_payload = true;

    assert!(writer.finish(&[], true, || Ok(())).is_err());
    assert_eq!(std::fs::read(recovery_file).unwrap(), b"original");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn preserve_never_replaces_an_existing_version_payload() {
    let root = test_root("preserve-collision");
    let old = root.join("kept.txt");
    std::fs::write(&old, b"original").unwrap();
    let local_root = LocalRoot::open(root.clone()).unwrap();
    let mut writer = VersionWriter::begin(&local_root, || Ok(())).unwrap();
    let retained = root
        .join(to_native(writer.version_dir.as_str()))
        .join("files/kept.txt");
    std::fs::create_dir_all(retained.parent().unwrap()).unwrap();
    std::fs::write(&retained, b"existing history").unwrap();
    let rel = RootRelativePath::new("kept.txt").unwrap();

    let error = writer
        .preserve(&rel, None, "test", false, || Ok(()))
        .unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read(old).unwrap(), b"original");
    assert_eq!(std::fs::read(retained).unwrap(), b"existing history");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn finish_propagates_index_publication_failure() {
    let root = test_root("finish-failure");
    let old = root.join("kept.txt");
    std::fs::write(&old, b"original").unwrap();
    let local_root = LocalRoot::open(root.clone()).unwrap();
    let mut writer = VersionWriter::begin(&local_root, || Ok(())).unwrap();
    let rel = RootRelativePath::new("kept.txt").unwrap();
    writer
        .preserve(&rel, None, "test", false, || Ok(()))
        .unwrap();
    std::fs::create_dir(
        root.join(crate::foundation::names::VERSION_STORE_DIR)
            .join("index.jsonl"),
    )
    .unwrap();

    assert!(writer.finish(&[], false, || Ok(())).is_err());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn prune_validates_every_id_before_deleting_anything() {
    let root = test_root("prune-traversal");
    let outside = root.parent().unwrap().join(format!(
        "syncdash-version-prune-sentinel-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&outside);
    std::fs::write(&outside, b"keep").unwrap();
    let store = root.join(crate::foundation::names::VERSION_STORE_DIR);
    std::fs::create_dir_all(&store).unwrap();
    let malicious_id = format!("../../{}", outside.file_name().unwrap().to_string_lossy());
    std::fs::write(
        store.join("index.jsonl"),
        format!(
            "{{\"id\":{},\"ts_ms\":1,\"host\":\"test\",\"ops\":1,\"preserved\":1,\"bytes\":4}}\n",
            serde_json::to_string(&malicious_id).unwrap()
        ),
    )
    .unwrap();

    assert!(prune(&root, 0).is_err());
    assert_eq!(std::fs::read(&outside).unwrap(), b"keep");
    let _ = std::fs::remove_file(outside);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn restore_validates_the_whole_manifest_before_mutating_destinations() {
    let root = test_root("restore-traversal");
    std::fs::write(root.join("kept.txt"), b"current").unwrap();
    let version_dir = root
        .join(crate::foundation::names::VERSION_STORE_DIR)
        .join("v1");
    std::fs::create_dir_all(version_dir.join("files")).unwrap();
    std::fs::write(version_dir.join("files/kept.txt"), b"archived").unwrap();
    let mut valid_entry = serde_json::to_value(whole_entry("kept.txt")).unwrap();
    let mut malicious_entry = valid_entry.clone();
    malicious_entry["rel"] = serde_json::Value::String("../outside.txt".to_owned());
    let manifest = serde_json::json!({
        "id": "v1",
        "ts_ms": 1,
        "host": "test",
        "entries": [valid_entry.take(), malicious_entry],
    });
    std::fs::write(
        version_dir.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();

    assert!(restore(&root, "v1", &[], false).is_err());
    assert_eq!(std::fs::read(root.join("kept.txt")).unwrap(), b"current");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn restore_refuses_an_unsafe_version_selector() {
    let root = test_root("version-selector");
    let error = restore(&root, "../v1", &[], true).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    let _ = std::fs::remove_dir_all(root);
}
