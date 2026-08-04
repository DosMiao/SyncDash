use super::*;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::path::Path;
use std::path::PathBuf;

// macos.rs contributes nothing on other platforms, so an ungated glob would be an unused
// import there. linux.rs and windows.rs both expose items to the cross-platform tests below.
use super::linux::*;
#[cfg(target_os = "macos")]
use super::macos::*;
use super::windows::*;

fn temp_root(tag: &str) -> PathBuf {
    let root =
        std::env::temp_dir().join(format!("syncdash-local-state-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("nested")).unwrap();
    root
}

#[test]
fn binding_is_unambiguous_across_volume_and_relative_root_boundaries() {
    assert_ne!(encode_binding(b"ab", b"c"), encode_binding(b"a", b"bc"));
    assert_ne!(
        encode_binding(b"volume-a", b"same"),
        encode_binding(b"volume-b", b"same")
    );
    assert_ne!(
        encode_binding(b"same", b"root-a"),
        encode_binding(b"same", b"root-b")
    );
}

#[test]
fn canonical_spellings_of_one_root_share_an_identity_but_siblings_do_not() {
    let root = temp_root("canonical");
    let direct = LocalScanStateIdentity::for_root(&root.join("nested"));
    let dotted = LocalScanStateIdentity::for_root(&root.join(".").join("nested"));
    let sibling = LocalScanStateIdentity::for_root(&root);
    assert_eq!(direct.binding(), dotted.binding());
    assert_ne!(direct.binding(), sibling.binding());
    let _ = std::fs::remove_dir_all(root);
}

/// Asserted against the production classifier this module now calls, so the guarantee protects the
/// scan-state binding rather than a copy of the table kept beside the test.
#[test]
fn fat_family_file_ids_are_never_called_stable() {
    use crate::fs::vfs::local::volume::{fat_family, file_ids_stable_for_fs};

    for name in ["FAT", "fat32", "msdos", "vfat", "exFAT"] {
        assert!(fat_family(name), "{name}");
        assert!(!file_ids_stable_for_fs(name), "{name}");
    }
    for name in ["apfs", "NTFS", "ext4", "smbfs", ""] {
        assert!(!fat_family(name), "{name}");
    }
    assert!(file_ids_stable_for_fs("apfs"));
    assert!(file_ids_stable_for_fs("NTFS"));
    assert!(file_ids_stable_for_fs("ext4"));
    assert!(!file_ids_stable_for_fs("exFAT"));
    assert!(!file_ids_stable_for_fs(""));
    assert!(!file_ids_stable_for_fs("mysteryfs"));
}

#[test]
fn linux_file_id_stability_is_a_positive_magic_allowlist() {
    for magic in [
        0x0000_ef53,
        0x9123_683e,
        0x5846_5342,
        0x2fc1_2fc1,
        0xf2f5_2010,
        0x3153_464a,
    ] {
        assert!(linux_file_ids_stable_magic(magic), "magic {magic:#x}");
    }
    for magic in [
        0x0000_4d44, // FAT
        0x2011_bab0, // exFAT
        0x794c_7630, // overlayfs
        0x0102_1994, // tmpfs
        0x6573_5546, // FUSE
        0xfeed_beef, // unknown/future
    ] {
        assert!(!linux_file_ids_stable_magic(magic), "magic {magic:#x}");
    }
}

#[test]
fn windows_path_normalization_folds_prefixes_separators_and_case() {
    assert_eq!(normalize_windows_path(r"\\?\D:\Code\"), "d:/code");
    assert_eq!(
        normalize_windows_path(r"\\?\UNC\Host\Share\Code"),
        "//host/share/code"
    );
}

#[test]
fn windows_requires_a_volume_guid_even_when_serial_and_mount_path_match() {
    let (first, first_durable) = windows_volume_identity(Some(r"\\?\Volume{AAAA}\"), Some(7), "e:");
    let (replacement, replacement_durable) =
        windows_volume_identity(Some(r"\\?\Volume{BBBB}\"), Some(7), "e:");
    let (unc_or_probe_failure, fallback_durable) = windows_volume_identity(None, Some(7), "e:");

    assert!(first_durable && replacement_durable);
    assert_ne!(first, replacement);
    assert!(!fallback_durable);
    assert!(unc_or_probe_failure.starts_with(b"windows-nondurable-root:"));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn linux_mountinfo_parser_decodes_paths_and_chooses_the_deepest_mount() {
    let mountinfo = b"not a mountinfo row\n\
25 1 8:1 / / rw,relatime - ext4 /dev/sda1 rw\n\
40 25 8:2 / /media/Backup\\040Disk rw,relatime - exfat /dev/sdb1 rw\n\
41 40 8:2 /nested /media/Backup\\040Disk/nested rw,relatime - exfat /dev/sdb1 rw\n";

    assert_eq!(
        mountinfo_mount_id(mountinfo, Path::new("/media/Backup Disk/file.txt")),
        Some(40)
    );
    assert_eq!(
        mountinfo_mount_id(
            mountinfo,
            Path::new("/media/Backup Disk/nested/deeper/file.txt")
        ),
        Some(41)
    );
    assert_eq!(
        decode_mountinfo_path(br"/media/has\134slash"),
        Some(PathBuf::from("/media/has\\slash"))
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn linux_prefers_a_persistent_filesystem_uuid_over_mount_numbers() {
    let first = linux_volume_identity(
        Some("A1B2-C3D4"),
        Some("boot=old;ns=1;mount=10;dev=8"),
        Some(8),
        b"first-process",
    );
    let same_filesystem_after_remount = linux_volume_identity(
        Some("a1b2-c3d4"),
        Some("boot=new;ns=2;mount=99;dev=42"),
        Some(42),
        b"second-process",
    );
    let replacement = linux_volume_identity(
        Some("ffff-0001"),
        Some("boot=old;ns=1;mount=10;dev=8"),
        Some(8),
        b"first-process",
    );

    assert_eq!(first, same_filesystem_after_remount);
    assert_ne!(first, replacement);
    assert!(first.starts_with(b"linux-fs-uuid:"));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn linux_fallbacks_fail_closed_when_st_dev_is_reused() {
    let mounted = linux_volume_identity(
        None,
        Some("boot=a;ns=1;mount=10;dev=8"),
        Some(8),
        b"same-process",
    );
    let replacement_mount = linux_volume_identity(
        None,
        Some("boot=a;ns=1;mount=11;dev=8"),
        Some(8),
        b"same-process",
    );
    let after_reboot = linux_volume_identity(
        None,
        Some("boot=b;ns=1;mount=10;dev=8"),
        Some(8),
        b"same-process",
    );
    let same_mount_from_another_process = linux_volume_identity(
        None,
        Some("boot=a;ns=1;mount=10;dev=8"),
        Some(8),
        b"another-process",
    );
    assert_eq!(mounted, same_mount_from_another_process);
    assert_ne!(mounted, replacement_mount);
    assert_ne!(mounted, after_reboot);

    let no_mount_evidence_a = linux_volume_identity(None, None, Some(8), b"process-a");
    let no_mount_evidence_b = linux_volume_identity(None, None, Some(8), b"process-b");
    assert_ne!(no_mount_evidence_a, no_mount_evidence_b);
    assert!(no_mount_evidence_a.starts_with(b"unix-ephemeral-volume\0"));
    assert!(!no_mount_evidence_a
        .windows(b"unix-dev:".len())
        .any(|part| part == b"unix-dev:"));

    // Ordinary mountinfo IDs and st_dev can both be reused after unmount. Even if their bytes
    // happen to collide, the durable gate prevents the old on-disk cache from being consulted.
    assert!(!linux_identity_is_durable(None, None));
    assert!(!linux_identity_is_durable(None, Some("")));
    assert!(linux_identity_is_durable(
        None,
        Some("boot=a;unique_mount=10;dev=8")
    ));
    assert!(linux_identity_is_durable(Some("A1B2-C3D4"), None));
}

#[cfg(target_os = "macos")]
#[test]
fn macos_uses_the_volume_uuid_when_the_filesystem_reports_one() {
    let root = temp_root("macos-uuid");
    let canonical = std::fs::canonicalize(&root).unwrap();
    let (mount, _) = macos_mount(&canonical).unwrap();
    let identity = platform_identity(&canonical);
    if macos_volume_uuid(&mount).is_some() {
        assert!(identity.volume.starts_with(b"macos-uuid:"));
        assert!(identity.persistent_reuse);
    } else {
        assert!(identity.volume.starts_with(b"macos-nondurable-dev:"));
        assert!(!identity.persistent_reuse);
    }
    let _ = std::fs::remove_dir_all(root);
}
