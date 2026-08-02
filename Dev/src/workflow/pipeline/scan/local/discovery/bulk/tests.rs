use super::super::walk::{WalkEntry, WalkKind, WalkStats};
use super::record::*;
use super::*;
use crate::fs::local_root::LocalRoot;
use crate::pipeline::filter::PathFilter;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn test_root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("syncdash-bulk-{tag}-{}", std::process::id()))
    }

    fn collect_bulk(root: &Path, filter: &PathFilter) -> (Vec<WalkEntry>, WalkStats) {
        let local_root = LocalRoot::open(root.to_path_buf()).unwrap();
        let mut entries = Vec::new();
        let stats = walk(&local_root, filter, || Ok(()), |entry| entries.push(entry)).unwrap();
        entries.sort_by(|left, right| left.relative.cmp(&right.relative));
        (entries, stats)
    }

    fn collect_reference(root: &Path, filter: &PathFilter) -> (Vec<WalkEntry>, WalkStats) {
        let local_root = LocalRoot::open(root.to_path_buf()).unwrap();
        let mut entries = Vec::new();
        let stats = crate::pipeline::scan::local::discovery::walk::walk(
            &local_root,
            filter,
            || Ok(()),
            |entry| entries.push(entry),
        )
        .unwrap();
        entries.sort_by(|left, right| left.relative.cmp(&right.relative));
        (entries, stats)
    }

    #[test]
    fn bulk_records_match_walkdir_metadata_and_filter_accounting() {
        let root = test_root("differential");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("keep/nested")).unwrap();
        std::fs::create_dir_all(root.join("pruned/child")).unwrap();
        std::fs::write(root.join("keep/a.txt"), b"alpha").unwrap();
        std::fs::write(root.join("keep/nested/empty.bin"), b"").unwrap();
        std::fs::write(root.join("excluded.bin"), b"excluded").unwrap();
        std::fs::write(root.join("pruned/child/hidden.txt"), b"hidden").unwrap();
        symlink("a.txt", root.join("keep/link")).unwrap();
        std::fs::set_permissions(
            root.join("keep/a.txt"),
            std::fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        filetime::set_file_mtime(
            root.join("keep/a.txt"),
            filetime::FileTime::from_unix_time(1_700_000_000, 987_654_321),
        )
        .unwrap();

        let filter =
            PathFilter::build(&[], &["/pruned/".to_string(), "/excluded.bin:".to_string()]);
        let (bulk_entries, bulk_stats) = collect_bulk(&root, &filter);
        let (reference_entries, reference_stats) = collect_reference(&root, &filter);

        assert_eq!(bulk_entries, reference_entries);
        assert_eq!(bulk_stats, reference_stats);
        assert_eq!(bulk_stats.excluded_dirs, 1);
        assert_eq!(bulk_stats.excluded_files, 1);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unsupported_root_bulk_call_falls_back_to_walkdir_with_parity() {
        let root = test_root("compatibility-fallback");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("keep/nested")).unwrap();
        std::fs::write(root.join("keep/a.txt"), b"alpha").unwrap();
        std::fs::write(root.join("keep/nested/b.txt"), b"beta").unwrap();
        std::fs::write(root.join("excluded.tmp"), b"ignored").unwrap();
        let filter = PathFilter::build(&[], &["*/*.tmp".to_string()]);
        let (reference_entries, reference_stats) = collect_reference(&root, &filter);

        for errno in [libc::ENOTSUP, libc::EINVAL] {
            let local_root = LocalRoot::open(root.clone()).unwrap();
            let calls = std::cell::Cell::new(0usize);
            let mut entries = Vec::new();
            let stats = walk_with_bulk(
                &local_root,
                &filter,
                || Ok(()),
                |entry| entries.push(entry),
                |_fd, _buffer| {
                    calls.set(calls.get() + 1);
                    Err(std::io::Error::from_raw_os_error(errno))
                },
            )
            .unwrap();
            entries.sort_by(|left, right| left.relative.cmp(&right.relative));

            assert_eq!(
                calls.get(),
                1,
                "fallback must abandon bulk enumeration immediately"
            );
            assert_eq!(entries, reference_entries, "errno {errno}");
            assert_eq!(stats, reference_stats, "errno {errno}");
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn compatibility_error_after_emission_never_restarts_the_walk() {
        let root = test_root("compatibility-after-emission");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("already-emitted.txt"), b"content").unwrap();
        let filter = PathFilter::build(&[], &[]);
        let calls = std::cell::Cell::new(0usize);
        let mut entries = Vec::new();
        let local_root = LocalRoot::open(root.clone()).unwrap();

        let error = walk_with_bulk(
            &local_root,
            &filter,
            || Ok(()),
            |entry| entries.push(entry),
            |fd, buffer| {
                let call = calls.get();
                calls.set(call + 1);
                if call == 0 {
                    system_bulk_read(fd, buffer)
                } else {
                    Err(std::io::Error::from_raw_os_error(libc::EINVAL))
                }
            },
        )
        .unwrap_err();

        assert!(
            !entries.is_empty(),
            "the first bulk batch must establish the no-restart boundary"
        );
        assert!(is_root_bulk_compatibility_error(&error));
        assert_eq!(calls.get(), 2);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn parser_keeps_non_utf8_names_raw() {
        let mut record = synthetic_file_record(&[b'b', 0xff, b'c']);
        let parsed = parse_record(&record).unwrap();
        let raw = std::ffi::OsString::from_vec(parsed.name.to_vec());
        assert!(raw.to_str().is_none());

        let last = record.len() - 1;
        record[last] = b'x';
        assert_eq!(
            parse_record(&record).unwrap_err(),
            "name is not exactly one null-terminated string"
        );
    }

    #[test]
    fn mountpoints_and_firmlinks_require_path_metadata() {
        let directory = |flags, mount_status| ParsedEntry {
            name: b"dir",
            entry_error: Some(0),
            dev: Some(1),
            kind: Some(WalkKind::Dir),
            mtime_ms: Some(1),
            mode: Some(0o755),
            flags: Some(flags),
            file_id: Some(2),
            mount_status: Some(mount_status),
            size: Some(0),
        };

        assert!(directory(0, 0).complete());
        assert!(!directory(0, DIR_MNTSTATUS_MNTPOINT).complete());
        assert!(!directory(SF_FIRMLINK, 0).complete());
    }

    fn synthetic_file_record(name: &[u8]) -> Vec<u8> {
        let mut record = Vec::new();
        record.extend_from_slice(&0u32.to_ne_bytes());
        let common = libc::ATTR_CMN_NAME
            | libc::ATTR_CMN_DEVID
            | libc::ATTR_CMN_OBJTYPE
            | libc::ATTR_CMN_MODTIME
            | libc::ATTR_CMN_ACCESSMASK
            | libc::ATTR_CMN_FLAGS
            | libc::ATTR_CMN_FILEID;
        record.extend_from_slice(&common.to_ne_bytes());
        record.extend_from_slice(&0u32.to_ne_bytes());
        record.extend_from_slice(&0u32.to_ne_bytes());
        record.extend_from_slice(&libc::ATTR_FILE_DATALENGTH.to_ne_bytes());
        record.extend_from_slice(&0u32.to_ne_bytes());

        let name_reference = record.len();
        record.extend_from_slice(&0i32.to_ne_bytes());
        record.extend_from_slice(&((name.len() + 1) as u32).to_ne_bytes());
        record.extend_from_slice(&7i32.to_ne_bytes());
        record.extend_from_slice(&1u32.to_ne_bytes());
        record.extend_from_slice(&1_700_000_000i64.to_ne_bytes());
        record.extend_from_slice(&987_654_321i64.to_ne_bytes());
        record.extend_from_slice(&0o640u32.to_ne_bytes());
        record.extend_from_slice(&0u32.to_ne_bytes());
        record.extend_from_slice(&42u64.to_ne_bytes());
        record.extend_from_slice(&5u64.to_ne_bytes());

        let name_start = record.len();
        record.extend_from_slice(name);
        record.push(0);
        let offset = (name_start - name_reference) as i32;
        record[name_reference..name_reference + 4].copy_from_slice(&offset.to_ne_bytes());
        let length = record.len() as u32;
        record[0..4].copy_from_slice(&length.to_ne_bytes());
        record
    }
}
