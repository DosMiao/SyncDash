use super::*;

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct TestRow {
    value: u32,
}

fn temp_file(tag: &str) -> std::path::PathBuf {
    let directory =
        std::env::temp_dir().join(format!("syncdash-store-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    directory.join("state.jsonl")
}

fn cleanup(file: &std::path::Path) {
    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}

#[test]
fn a_failed_cache_rewrite_leaves_the_previous_generation_intact() {
    let destination = temp_file("atomic");
    std::fs::write(&destination, b"previous\n").unwrap();

    let result = super::super::atomic::rewrite(&destination, |writer| {
        writer.write_all(b"incomplete\n")?;
        Err(std::io::Error::other("injected failure"))
    });

    assert!(result.is_err());
    assert_eq!(std::fs::read(&destination).unwrap(), b"previous\n");
    cleanup(&destination);
}

#[test]
fn a_failed_versioned_rewrite_also_leaves_the_previous_generation_intact() {
    let destination = temp_file("versioned-atomic");
    std::fs::write(&destination, b"{\"value\":1}\n").unwrap();

    let result = rewrite(&destination, "test", b"root", |writer| {
        serde_json::to_writer(&mut *writer, &TestRow { value: 2 })
            .map_err(std::io::Error::other)?;
        writer.write_all(b"\n")?;
        Err(std::io::Error::other("injected failure"))
    });

    assert!(result.is_err());
    assert_eq!(std::fs::read(&destination).unwrap(), b"{\"value\":1}\n");
    cleanup(&destination);
}

#[test]
fn an_unknown_state_version_is_neither_loaded_nor_downgraded() {
    let destination = temp_file("future");
    let original = format!(
        "{}\n{{\"value\":7}}\n",
        serde_json::json!({
            "schema": SCHEMA,
            "version": VERSION + 1,
            "kind": "test",
            "root_binding": binding::digest(b"root"),
        })
    );
    std::fs::write(&destination, original.as_bytes()).unwrap();

    let mut rows = Vec::new();
    assert_eq!(
        read(&destination, "test", b"root", |row: TestRow| {
            rows.push(row.value);
        })
        .unwrap(),
        ReadStatus::Rejected
    );
    assert!(rows.is_empty());
    assert!(!needs_rebuild(&destination, "test", b"root"));
    assert!(!rewrite(&destination, "test", b"root", |_| Ok(())).unwrap());
    assert_eq!(std::fs::read_to_string(&destination).unwrap(), original);
    cleanup(&destination);
}
