//! macOS directory enumeration with `getattrlistbulk(2)`.
//!
//! One call returns names and stat-equivalent metadata for many children. This removes the
//! `readdir` + `lstat` syscall pair per item that dominates trees containing hundreds of thousands
//! of small files, while preserving the local scanner's strict path and error contracts.

use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use crate::foundation::path::{EntryName, RootRelativeDir, RootRelativePath};
use crate::fs::local_root::LocalRoot;
use crate::pipeline::filter::PathFilter;

use super::walk::{WalkEntry, WalkKind, WalkStats};

const BUFFER_SIZE: usize = 1024 * 1024;
const ATTR_CMN_ERROR: u32 = 0x2000_0000;
const VDIR: u32 = 2;
const VLNK: u32 = 5;
const SF_FIRMLINK: u32 = 0x0080_0000;
const SF_DATALESS: u32 = 0x4000_0000;
const DIR_MNTSTATUS_MNTPOINT: u32 = 0x0000_0001;

const REQUESTED_COMMON: u32 = libc::ATTR_CMN_RETURNED_ATTRS
    | libc::ATTR_CMN_NAME
    | libc::ATTR_CMN_DEVID
    | libc::ATTR_CMN_OBJTYPE
    | libc::ATTR_CMN_MODTIME
    | libc::ATTR_CMN_ACCESSMASK
    | libc::ATTR_CMN_FLAGS
    | libc::ATTR_CMN_FILEID
    | ATTR_CMN_ERROR;

static ATTRS: libc::attrlist = libc::attrlist {
    bitmapcount: libc::ATTR_BIT_MAP_COUNT,
    reserved: 0,
    commonattr: REQUESTED_COMMON,
    volattr: 0,
    dirattr: libc::ATTR_DIR_MOUNTSTATUS | libc::ATTR_DIR_DATALENGTH,
    fileattr: libc::ATTR_FILE_DATALENGTH,
    forkattr: 0,
};

#[derive(Debug)]
struct ParsedEntry<'a> {
    name: &'a [u8],
    entry_error: Option<u32>,
    dev: Option<u64>,
    kind: Option<WalkKind>,
    mtime_ms: Option<i64>,
    mode: Option<u32>,
    flags: Option<u32>,
    file_id: Option<u64>,
    mount_status: Option<u32>,
    size: Option<u64>,
}

impl ParsedEntry<'_> {
    fn complete(&self) -> bool {
        // Bulk metadata describes the covered directory/firmlink object, not the mounted/firmlink
        // root exposed through its descriptor. Force uncommon entries through descriptor-relative
        // metadata so mounted and firmlink roots keep their visible identity.
        self.entry_error.unwrap_or(0) == 0
            && self.dev.is_some()
            && self.kind.is_some()
            && self.mtime_ms.is_some()
            && self.mode.is_some()
            && self.flags.is_some()
            && self.file_id.is_some()
            && self.size.is_some()
            && match self.kind {
                Some(WalkKind::Dir) => matches!(
                    (self.mount_status, self.flags),
                    (Some(status), Some(flags))
                        if status & DIR_MNTSTATUS_MNTPOINT == 0
                            && flags & SF_FIRMLINK == 0
                ),
                Some(WalkKind::File | WalkKind::Symlink) => true,
                None => false,
            }
    }

    fn into_walk_entry(self, relative: RootRelativePath) -> WalkEntry {
        let dev = self.dev.expect("complete bulk entry has a device id");
        let file_id = self.file_id.expect("complete bulk entry has a file id");
        WalkEntry {
            relative,
            kind: self.kind.expect("complete bulk entry has a kind"),
            size: self.size.expect("complete bulk entry has a size"),
            mtime_ms: self.mtime_ms.expect("complete bulk entry has an mtime"),
            file_id: Some(format!("{dev}:{file_id}")),
            mode: Some(self.mode.expect("complete bulk entry has a mode") & 0o7777),
            dataless: self.flags.expect("complete bulk entry has flags") & SF_DATALESS != 0,
        }
    }
}

#[derive(Debug)]
struct RootBulkCompatibilityError {
    root: PathBuf,
    source: std::io::Error,
}

impl std::fmt::Display for RootBulkCompatibilityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "scan of '{}' could not use bulk directory metadata: {}",
            self.root.display(),
            self.source
        )
    }
}

impl std::error::Error for RootBulkCompatibilityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

fn is_root_bulk_compatibility_error(error: &std::io::Error) -> bool {
    error
        .get_ref()
        .and_then(|source| source.downcast_ref::<RootBulkCompatibilityError>())
        .is_some()
}

fn system_bulk_read(fd: RawFd, buffer: &mut [u8]) -> std::io::Result<usize> {
    let count = unsafe {
        // SAFETY: `fd` names a readable directory, ATTRS is fully initialized, and `buffer` is
        // valid writable memory for the supplied length.
        libc::getattrlistbulk(
            fd,
            &ATTRS as *const libc::attrlist as *mut libc::c_void,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            libc::FSOPT_NOFOLLOW as u64,
        )
    };
    if count < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(count as usize)
    }
}

pub(super) fn walk<C, V>(
    root: &LocalRoot,
    filter: &PathFilter,
    checkpoint: C,
    visit: V,
) -> std::io::Result<WalkStats>
where
    C: FnMut() -> std::io::Result<()>,
    V: FnMut(WalkEntry),
{
    walk_with_bulk(root, filter, checkpoint, visit, system_bulk_read)
}

fn walk_with_bulk<C, V, B>(
    root: &LocalRoot,
    filter: &PathFilter,
    mut checkpoint: C,
    mut visit: V,
    mut bulk_read: B,
) -> std::io::Result<WalkStats>
where
    C: FnMut() -> std::io::Result<()>,
    V: FnMut(WalkEntry),
    B: FnMut(RawFd, &mut [u8]) -> std::io::Result<usize>,
{
    let mut emitted = false;
    let result = {
        let mut emit = |entry| {
            emitted = true;
            visit(entry);
        };
        walk_bulk(root, filter, &mut checkpoint, &mut emit, &mut bulk_read)
    };

    match result {
        Err(error) if !emitted && is_root_bulk_compatibility_error(&error) => {
            crate::log_warn!(
                "scan",
                "{}; falling back to the compatible WalkDir enumerator before publishing any entries",
                error
            );
            super::walk::walk(root, filter, checkpoint, visit)
        }
        result => result,
    }
}

fn walk_bulk<C, V, B>(
    root: &LocalRoot,
    filter: &PathFilter,
    checkpoint: &mut C,
    visit: &mut V,
    bulk_read: &mut B,
) -> std::io::Result<WalkStats>
where
    C: FnMut() -> std::io::Result<()>,
    V: FnMut(WalkEntry),
    B: FnMut(RawFd, &mut [u8]) -> std::io::Result<usize>,
{
    let mut stats = WalkStats::default();
    let mut directories = vec![RootRelativeDir::new("").expect("the root directory is valid")];
    let mut buffer = vec![0u8; BUFFER_SIZE];

    while let Some(directory_relative) = directories.pop() {
        checkpoint()?;
        let directory = match root.open_directory(&directory_relative) {
            Ok(directory) => directory,
            Err(error) if directory_relative.as_str().is_empty() => {
                return Err(std::io::Error::new(
                    error.kind(),
                    format!(
                        "scan of '{}' could not read the root itself: {error} — refusing to report it as an empty tree (that reads as a mass deletion on the other side)",
                        root.display_path().display()
                    ),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                stats.note_error(format!("{}: {error}", directory_relative.as_str()));
                continue;
            }
            Err(error) => return Err(subtree_error(root, directory_relative.as_str(), error)),
        };
        let directory_handle = directory.try_clone_handle()?;

        loop {
            checkpoint()?;
            let count = match bulk_read(directory_handle.as_raw_fd(), &mut buffer) {
                Ok(count) => count,
                Err(error)
                    if directory_relative.as_str().is_empty()
                        && matches!(
                            error.raw_os_error(),
                            Some(libc::ENOTSUP) | Some(libc::EINVAL)
                        ) =>
                {
                    return Err(std::io::Error::new(
                        error.kind(),
                        RootBulkCompatibilityError {
                            root: root.display_path().to_path_buf(),
                            source: error,
                        },
                    ));
                }
                Err(error) => {
                    if directory_relative.as_str().is_empty() {
                        return Err(std::io::Error::new(
                        error.kind(),
                        format!(
                            "scan of '{}' could not read the root itself: {error} — refusing to report it as an empty tree (that reads as a mass deletion on the other side)",
                            root.display_path().display()
                        ),
                    ));
                    }
                    if error.kind() == std::io::ErrorKind::NotFound {
                        stats.note_error(format!("{}: {error}", directory_relative.as_str()));
                        break;
                    }
                    return Err(subtree_error(root, directory_relative.as_str(), error));
                }
            };
            if count == 0 {
                break;
            }

            let mut offset = 0usize;
            for _ in 0..count {
                checkpoint()?;
                let record = record_at(&buffer, offset).map_err(|error| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "scan of '{}' received malformed directory metadata at '{}': {error} — refusing to emit a half table",
                            root.display_path().display(),
                            directory_relative.as_str()
                        ),
                    )
                })?;
                offset = offset
                    .checked_add(record.len())
                    .ok_or_else(|| std::io::Error::other("bulk record offset overflow"))?;
                let parsed = parse_record(record).map_err(|error| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "scan of '{}' received malformed directory metadata at '{}': {error} — refusing to emit a half table",
                            root.display_path().display(),
                            directory_relative.as_str()
                        ),
                    )
                })?;
                if parsed.name == b"." || parsed.name == b".." {
                    continue;
                }

                let name = std::ffi::OsString::from_vec(parsed.name.to_vec());
                let Some(name_text) = name.to_str() else {
                    stats.note_invalid_name(Path::new(&name));
                    continue;
                };
                let name = match EntryName::new(name_text) {
                    Ok(name) => name,
                    Err(error) => {
                        stats.note_error(error.to_string());
                        continue;
                    }
                };
                let relative = child_path(&directory_relative, &name);
                let mut fallback_metadata = None;
                let kind = match parsed.kind {
                    Some(kind) => kind,
                    None => match root.metadata_path(&relative) {
                        Ok(metadata) => {
                            let kind = super::walk::kind_from_metadata(&metadata);
                            fallback_metadata = Some(metadata);
                            kind
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            stats.note_error(format!("{}: {error}", relative.as_str()));
                            continue;
                        }
                        Err(error) => return Err(subtree_error(root, relative.as_str(), error)),
                    },
                };

                let keep = match kind {
                    WalkKind::Dir => {
                        let (pass, child_might_match) = filter.pass_dir(relative.as_str());
                        let keep = pass || child_might_match;
                        if !keep {
                            stats.excluded_dirs += 1;
                        }
                        keep
                    }
                    WalkKind::File | WalkKind::Symlink => {
                        let keep = filter.pass_file(relative.as_str());
                        if !keep {
                            stats.excluded_files += 1;
                        }
                        keep
                    }
                };
                if !keep {
                    continue;
                }

                let entry = if parsed.complete() {
                    parsed.into_walk_entry(relative.clone())
                } else {
                    let metadata = match fallback_metadata {
                        Some(metadata) => metadata,
                        None => match root.metadata_path(&relative) {
                            Ok(metadata) => metadata,
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                                stats.note_error(format!("{}: {error}", relative.as_str()));
                                if kind == WalkKind::Dir {
                                    directories.push(as_directory(relative.clone()));
                                }
                                continue;
                            }
                            Err(error) if kind != WalkKind::Dir => {
                                stats.note_error(format!("{}: {error}", relative.as_str()));
                                continue;
                            }
                            Err(error) => {
                                return Err(subtree_error(root, relative.as_str(), error));
                            }
                        },
                    };
                    let dataless = if kind == WalkKind::File {
                        root.is_dataless_file(&relative)?
                    } else {
                        false
                    };
                    WalkEntry::from_metadata(relative.clone(), &metadata, dataless)
                };
                visit(entry);

                if kind == WalkKind::Dir {
                    directories.push(as_directory(relative));
                }
            }
        }
    }

    Ok(stats)
}

fn child_path(directory: &RootRelativeDir, name: &EntryName) -> RootRelativePath {
    let relative = if directory.as_str().is_empty() {
        name.as_str().to_owned()
    } else {
        format!("{}/{}", directory.as_str(), name.as_str())
    };
    RootRelativePath::new(relative).expect("validated directory and entry names form a valid path")
}

fn as_directory(relative: RootRelativePath) -> RootRelativeDir {
    RootRelativeDir::new(relative.into_string())
        .expect("a validated child path is a valid directory path")
}

fn subtree_error(root: &LocalRoot, relative: &str, error: std::io::Error) -> std::io::Error {
    std::io::Error::new(
        error.kind(),
        format!(
            "scan of '{}' aborted at '{relative}': {error} — refusing to emit a half table (its missing subtrees would read as deletions)",
            root.display_path().display()
        ),
    )
}

fn record_at(buffer: &[u8], offset: usize) -> Result<&[u8], &'static str> {
    let length_end = offset.checked_add(4).ok_or("record offset overflow")?;
    let length_bytes = buffer
        .get(offset..length_end)
        .ok_or("record length is truncated")?;
    let length = u32::from_ne_bytes(
        length_bytes
            .try_into()
            .map_err(|_| "invalid record length")?,
    ) as usize;
    if length < 24 {
        return Err("record is shorter than its length and returned-attribute fields");
    }
    let end = offset.checked_add(length).ok_or("record length overflow")?;
    buffer
        .get(offset..end)
        .ok_or("record extends beyond the returned buffer")
}

fn parse_record(record: &[u8]) -> Result<ParsedEntry<'_>, &'static str> {
    let mut pos = 4usize;
    let returned = take(record, &mut pos, 20)?;
    let returned_common = read_u32(&returned[0..4])?;
    let returned_dir = read_u32(&returned[8..12])?;
    let returned_file = read_u32(&returned[12..16])?;
    if returned_common & !REQUESTED_COMMON != 0 {
        return Err("kernel returned an unrequested common attribute");
    }
    if returned_dir & !(libc::ATTR_DIR_MOUNTSTATUS | libc::ATTR_DIR_DATALENGTH) != 0 {
        return Err("kernel returned an unrequested directory attribute");
    }
    if returned_file & !libc::ATTR_FILE_DATALENGTH != 0 {
        return Err("kernel returned an unrequested file attribute");
    }

    let entry_error = if returned_common & ATTR_CMN_ERROR != 0 {
        Some(read_u32(take(record, &mut pos, 4)?)?)
    } else {
        None
    };

    let name = if returned_common & libc::ATTR_CMN_NAME != 0 {
        let reference_pos = pos;
        let reference = take(record, &mut pos, 8)?;
        let data_offset = i32::from_ne_bytes(
            reference[0..4]
                .try_into()
                .map_err(|_| "invalid name offset")?,
        );
        let data_length = read_u32(&reference[4..8])? as usize;
        if data_offset < 0 || data_length == 0 {
            return Err("invalid name reference");
        }
        let start = reference_pos
            .checked_add(data_offset as usize)
            .ok_or("name offset overflow")?;
        let end = start
            .checked_add(data_length)
            .ok_or("name length overflow")?;
        let name = record
            .get(start..end)
            .ok_or("name extends beyond its record")?;
        if name.last() != Some(&0) || name[..name.len() - 1].contains(&0) {
            return Err("name is not exactly one null-terminated string");
        }
        &name[..name.len() - 1]
    } else {
        return Err("required name attribute was not returned");
    };

    let dev = if returned_common & libc::ATTR_CMN_DEVID != 0 {
        let raw = read_i32(take(record, &mut pos, 4)?)?;
        Some(raw as u32 as u64)
    } else {
        None
    };
    let kind = if returned_common & libc::ATTR_CMN_OBJTYPE != 0 {
        Some(match read_u32(take(record, &mut pos, 4)?)? {
            VDIR => WalkKind::Dir,
            VLNK => WalkKind::Symlink,
            _ => WalkKind::File,
        })
    } else {
        None
    };
    let mtime_ms = if returned_common & libc::ATTR_CMN_MODTIME != 0 {
        let timespec = take(record, &mut pos, 16)?;
        Some(timespec_millis(
            read_i64(&timespec[0..8])?,
            read_i64(&timespec[8..16])?,
        ))
    } else {
        None
    };
    let mode = if returned_common & libc::ATTR_CMN_ACCESSMASK != 0 {
        Some(read_u32(take(record, &mut pos, 4)?)?)
    } else {
        None
    };
    let flags = if returned_common & libc::ATTR_CMN_FLAGS != 0 {
        Some(read_u32(take(record, &mut pos, 4)?)?)
    } else {
        None
    };
    let file_id = if returned_common & libc::ATTR_CMN_FILEID != 0 {
        Some(read_u64(take(record, &mut pos, 8)?)?)
    } else {
        None
    };
    let mount_status = if returned_dir & libc::ATTR_DIR_MOUNTSTATUS != 0 {
        Some(read_u32(take(record, &mut pos, 4)?)?)
    } else {
        None
    };
    let dir_size = if returned_dir & libc::ATTR_DIR_DATALENGTH != 0 {
        Some(read_u64(take(record, &mut pos, 8)?)?)
    } else {
        None
    };
    let file_size = if returned_file & libc::ATTR_FILE_DATALENGTH != 0 {
        Some(read_u64(take(record, &mut pos, 8)?)?)
    } else {
        None
    };

    Ok(ParsedEntry {
        name,
        entry_error,
        dev,
        kind,
        mtime_ms,
        mode,
        flags,
        file_id,
        mount_status,
        size: dir_size.or(file_size),
    })
}

fn take<'a>(record: &'a [u8], pos: &mut usize, width: usize) -> Result<&'a [u8], &'static str> {
    let end = pos.checked_add(width).ok_or("attribute offset overflow")?;
    let field = record.get(*pos..end).ok_or("attribute is truncated")?;
    *pos = end;
    Ok(field)
}

fn read_u32(bytes: &[u8]) -> Result<u32, &'static str> {
    Ok(u32::from_ne_bytes(
        bytes.try_into().map_err(|_| "invalid u32 attribute")?,
    ))
}

fn read_i32(bytes: &[u8]) -> Result<i32, &'static str> {
    Ok(i32::from_ne_bytes(
        bytes.try_into().map_err(|_| "invalid i32 attribute")?,
    ))
}

fn read_u64(bytes: &[u8]) -> Result<u64, &'static str> {
    Ok(u64::from_ne_bytes(
        bytes.try_into().map_err(|_| "invalid u64 attribute")?,
    ))
}

fn read_i64(bytes: &[u8]) -> Result<i64, &'static str> {
    Ok(i64::from_ne_bytes(
        bytes.try_into().map_err(|_| "invalid i64 attribute")?,
    ))
}

fn timespec_millis(seconds: i64, nanos: i64) -> i64 {
    if seconds < 0 || !(0..1_000_000_000).contains(&nanos) {
        return 0;
    }
    (seconds as i128 * 1000 + nanos as i128 / 1_000_000) as i64
}

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
        let stats =
            super::super::walk::walk(&local_root, filter, || Ok(()), |entry| entries.push(entry))
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
