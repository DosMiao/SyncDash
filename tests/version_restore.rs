use std::path::{Path, PathBuf};

use syncdash::foundation::path::RootRelativePath;
use syncdash::fs::local_root::LocalRoot;
use syncdash::store::version::{
    restore, PreservedEntry, VersionManifest, VersionPayloadKind, VersionWriter,
};

fn test_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "syncdash-version-restore-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn restore_sessions(root: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(
        root.join(syncdash::foundation::names::APP_DIR)
            .join("restore"),
    )
    .unwrap()
    .map(|entry| entry.unwrap().path())
    .collect()
}

fn whole_entry(relative: &str) -> PreservedEntry {
    PreservedEntry {
        relative_path: RootRelativePath::try_from(relative).unwrap(),
        payload_kind: VersionPayloadKind::Whole,
        reason: "integration test".to_owned(),
        old_hash: blake3::hash(b"archived").to_hex().to_string(),
        old_size: b"archived".len() as u64,
        old_mtime_ms: 0,
        old_mode: None,
        new_hash: None,
        recipe: None,
    }
}

fn reverse_delta_fixture(tag: &str) -> (PathBuf, PathBuf, Vec<u8>, Vec<u8>, String) {
    let root = test_root(tag);
    let newer_root_path = test_root(&format!("{tag}-newer"));
    let relative = RootRelativePath::try_from("large.bin").unwrap();
    let old = vec![7u8; syncdash::model::chunk::DELTA_MIN_SIZE as usize + 4096];
    let mut newer = old.clone();
    newer[1_000_000..1_004_096].fill(9);
    std::fs::write(root.join("large.bin"), &old).unwrap();
    std::fs::write(newer_root_path.join("large.bin"), &newer).unwrap();
    let local_root = LocalRoot::open(root.clone()).unwrap();
    let newer_root = LocalRoot::open(newer_root_path.clone()).unwrap();
    let mut writer = VersionWriter::begin(&local_root, || Ok(())).unwrap();
    writer
        .preserve(
            &relative,
            Some((&newer_root, &relative)),
            "test",
            true,
            || Ok(()),
        )
        .unwrap();
    std::fs::write(root.join("large.bin"), &newer).unwrap();
    let version = writer.finish(&[], true, || Ok(())).unwrap().unwrap();
    (root, newer_root_path, old, newer, version)
}

#[test]
fn payload_preflight_finishes_before_the_first_destination_changes() {
    let root = test_root("payload-preflight");
    for relative in ["first.txt", "second.txt"] {
        std::fs::write(root.join(relative), format!("current {relative}")).unwrap();
    }
    let version_directory = root
        .join(syncdash::foundation::names::VERSION_STORE_DIR)
        .join("v1");
    std::fs::create_dir_all(version_directory.join("files")).unwrap();
    std::fs::write(version_directory.join("files/first.txt"), b"archived").unwrap();
    std::fs::write(version_directory.join("files/second.txt"), b"corrupt!").unwrap();
    let manifest = VersionManifest {
        id: syncdash::foundation::path::EntryName::try_from("v1").unwrap(),
        ts_ms: 1,
        host: "test".to_owned(),
        entries: vec![whole_entry("first.txt"), whole_entry("second.txt")],
    };
    std::fs::write(
        version_directory.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();

    assert!(restore(&root, "v1", &[], false).is_err());
    assert_eq!(
        std::fs::read(root.join("first.txt")).unwrap(),
        b"current first.txt"
    );
    assert_eq!(
        std::fs::read(root.join("second.txt")).unwrap(),
        b"current second.txt"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn a_symlink_is_restored_as_a_link_without_reading_its_target() {
    use std::os::unix::fs::symlink;

    let root = test_root("symlink");
    let outside = root.with_file_name(format!(
        "syncdash-version-restore-outside-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&outside);
    std::fs::write(&outside, b"outside secret").unwrap();
    symlink(&outside, root.join("link.txt")).unwrap();
    let local_root = LocalRoot::open(root.clone()).unwrap();
    let relative = RootRelativePath::try_from("link.txt").unwrap();
    let mut writer = VersionWriter::begin(&local_root, || Ok(())).unwrap();
    writer
        .preserve(&relative, None, "test", true, || Ok(()))
        .unwrap();
    std::fs::write(root.join("link.txt"), b"current occupant").unwrap();
    let version = writer.finish(&[], true, || Ok(())).unwrap().unwrap();

    assert_eq!(restore(&root, &version, &[], false).unwrap(), (1, 0, 0));
    assert!(std::fs::symlink_metadata(root.join("link.txt"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(std::fs::read_link(root.join("link.txt")).unwrap(), outside);
    let sessions = restore_sessions(&root);
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        std::fs::read(sessions[0].join("link.txt")).unwrap(),
        b"current occupant"
    );
    let _ = std::fs::remove_file(outside);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn reverse_delta_is_reconstructed_before_its_base_is_displaced() {
    let (root, newer_root_path, old, newer, version) = reverse_delta_fixture("reverse-delta");

    assert_eq!(restore(&root, &version, &[], false).unwrap(), (1, 0, 0));
    assert_eq!(std::fs::read(root.join("large.bin")).unwrap(), old);
    let sessions = restore_sessions(&root);
    assert_eq!(sessions.len(), 1);
    assert_eq!(std::fs::read(sessions[0].join("large.bin")).unwrap(), newer);
    let _ = std::fs::remove_dir_all(newer_root_path);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn reverse_delta_rejects_unreferenced_blob_bytes_before_mutation() {
    let (root, newer_root_path, _, newer, version) =
        reverse_delta_fixture("reverse-delta-extra-blob");
    let blob = root
        .join(syncdash::foundation::names::VERSION_STORE_DIR)
        .join(&version)
        .join("rdelta/large.bin");
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(blob)
        .unwrap()
        .write_all(b"unreferenced")
        .unwrap();

    assert!(restore(&root, &version, &[], false).is_err());
    assert_eq!(std::fs::read(root.join("large.bin")).unwrap(), newer);
    assert!(!root.join(syncdash::foundation::names::APP_DIR).exists());
    let _ = std::fs::remove_dir_all(newer_root_path);
    let _ = std::fs::remove_dir_all(root);
}
