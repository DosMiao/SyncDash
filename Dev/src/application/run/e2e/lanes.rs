use std::sync::Arc;

use super::*;
use crate::fs::vfs::conformance;
use crate::fs::vfs::local::LocalVfs;
use crate::fs::vfs::memory::MemVfs;
use crate::fs::vfs::{Support, Vfs};

#[test]
fn memory_pipeline_smoke() {
    let source = Arc::new(MemVfs::new("e2e-memory-source")) as Arc<dyn Vfs>;
    let target = Arc::new(MemVfs::new("e2e-memory-target")) as Arc<dyn Vfs>;
    run_pipeline_smoke("memory", &source, &target, None);
}

#[test]
fn local_pipeline_smoke() {
    let base = std::env::temp_dir().join(format!("syncdash-e2e-{}", std::process::id()));
    let source_path = base.join("source");
    let target_path = base.join("target");
    let trash_path = base.join("trash");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&source_path).unwrap();
    std::fs::create_dir_all(&target_path).unwrap();

    let source = Arc::new(LocalVfs::open(source_path).unwrap()) as Arc<dyn Vfs>;
    let target = Arc::new(LocalVfs::open(target_path).unwrap()) as Arc<dyn Vfs>;
    run_pipeline_smoke("local", &source, &target, Some(trash_path));

    let _ = std::fs::remove_dir_all(base);
}

fn sftp_shaped(name: &str) -> Arc<dyn Vfs> {
    Arc::new(MemVfs::new(name).without(|capabilities| {
        capabilities.mtime_precision_ms = 1_000;
        capabilities.rename_overwrite = Support::No;
        capabilities.fsync = Support::Unknown;
        capabilities.free_space = Support::No;
        capabilities.write_at = Support::No;
    })) as Arc<dyn Vfs>
}

#[test]
fn sftp_shaped_pipeline_smoke() {
    let source = sftp_shaped("e2e-sftp-source");
    let target = sftp_shaped("e2e-sftp-target");
    run_pipeline_smoke("sftp-shaped", &source, &target, None);
}

fn ftp_list_only(name: &str) -> Arc<dyn Vfs> {
    Arc::new(MemVfs::new(name).without(|capabilities| {
        capabilities.mtime_precision_ms = 60_000;
        capabilities.ranged_read = Support::No;
        capabilities.set_mtime = Support::No;
        capabilities.symlink = Support::No;
        capabilities.unix_mode = Support::No;
        capabilities.read_back = Support::No;
        capabilities.free_space = Support::No;
        capabilities.write_at = Support::No;
        capabilities.rename_overwrite = Support::Unknown;
        capabilities.exclusive_staged_file_publish = Support::No;
    })) as Arc<dyn Vfs>
}

#[test]
fn ftp_list_only_compares_but_refuses_before_write() {
    let source = ftp_list_only("e2e-ftp-source");
    let target = ftp_list_only("e2e-ftp-target");
    corpus::seed_into(&source, corpus::BASE);
    corpus::seed_into(&target, corpus::BASE);
    corpus::write_seed(
        &source,
        Seed {
            path: "new.txt",
            seed: 5,
            size: 512,
            mtime_ms: 1_767_225_600_000,
        },
    );

    let job = bare_job();
    let (transcript, context) = watched();
    let comparison = crate::run::local::compare_resolved(&job, &source, &target, &context)
        .expect("a read-only backend must still compare");
    assert_eq!(comparison.plan.ops.len(), 1);
    assert_eq!(
        comparison.source.header.evidence,
        crate::model::table::TableEvidence::Full
    );

    let outcome = crate::run::local::apply_resolved(
        &job,
        &comparison.plan,
        &comparison.plan.ops,
        &source,
        &target,
        None,
        false,
        std::time::Instant::now(),
        &context,
    );
    assert_eq!((outcome.done, outcome.errors), (0, 1));
    let text = transcript.text();
    assert!(
        text.contains("root lock"),
        "the missing exclusive publication must still be listed before the run: {text}"
    );
}

fn live_lane(lane: &str, base_url: &str, pipeline_required: bool) {
    let open = |url: &str| -> Arc<dyn Vfs> {
        let root =
            crate::fs::vfs::open(url).unwrap_or_else(|error| panic!("opening '{url}': {error}"));
        root.connect()
            .unwrap_or_else(|error| panic!("connecting to '{url}': {error}"));
        root
    };

    let base = open(base_url);
    let mut roots = Vec::new();
    let mut serial = 0usize;
    let mut fresh_root = || {
        serial += 1;
        let name = format!("e2e-{}-{serial}", std::process::id());
        conformance::remove_tree(&base, &name)
            .unwrap_or_else(|error| panic!("clearing '{name}': {error}"));
        base.mkdir_all(&name)
            .unwrap_or_else(|error| panic!("creating '{name}': {error}"));
        roots.push(name.clone());
        open(&format!("{base_url}/{name}"))
    };

    let source = fresh_root();
    let target = fresh_root();
    let pipeline_supported = target.caps().exclusive_staged_file_publish.yes();
    assert!(
        !pipeline_required || pipeline_supported,
        "{lane} must support exclusive staged-file publication"
    );
    if pipeline_supported {
        run_pipeline_smoke(lane, &source, &target, None);
    }
    conformance::run_all(&mut fresh_root);

    for name in roots {
        conformance::remove_tree(&base, &name)
            .unwrap_or_else(|error| panic!("cleaning up '{name}': {error}"));
    }
}

#[test]
#[ignore = "needs a live SFTP server in SYNCDASH_E2E_SFTP_URL"]
fn sftp_live_lane() {
    let url = std::env::var("SYNCDASH_E2E_SFTP_URL")
        .expect("set SYNCDASH_E2E_SFTP_URL to an sftp://user@host/scratch phrase");
    live_lane("sftp", &url, true);
}

#[test]
#[ignore = "needs a live SMB share in SYNCDASH_E2E_SMB_URL and a stored credential"]
fn smb_live_lane() {
    let url = std::env::var("SYNCDASH_E2E_SMB_URL")
        .expect("set SYNCDASH_E2E_SMB_URL to an smb://user@host/share/scratch phrase");
    live_lane("smb", &url, true);
}

#[test]
#[ignore = "needs a live FTP server in SYNCDASH_E2E_FTP_URL"]
fn ftp_live_lane() {
    let url = std::env::var("SYNCDASH_E2E_FTP_URL")
        .expect("set SYNCDASH_E2E_FTP_URL to an ftp://user@host/scratch phrase");
    live_lane("ftp", &url, false);
}

#[test]
#[ignore = "needs a live FTPS server in SYNCDASH_E2E_FTPS_URL whose CA this machine trusts"]
fn ftps_live_lane() {
    let url = std::env::var("SYNCDASH_E2E_FTPS_URL")
        .expect("set SYNCDASH_E2E_FTPS_URL to an ftps://user@host/scratch phrase");
    live_lane("ftps", &url, false);
}

#[test]
#[ignore = "needs a writable directory on a non-temp volume in SYNCDASH_E2E_EXFAT_ROOT"]
fn exfat_live_lane() {
    let root = std::env::var("SYNCDASH_E2E_EXFAT_ROOT")
        .expect("set SYNCDASH_E2E_EXFAT_ROOT to a writable directory on the volume under test");
    let base = std::path::PathBuf::from(&root);
    std::fs::create_dir_all(&base).expect("the scratch root must be creatable");

    let mut directories = Vec::new();
    let mut serial = 0usize;
    let mut fresh_root = || {
        serial += 1;
        let path = base.join(format!("e2e-{}-{serial}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        directories.push(path.clone());
        Arc::new(LocalVfs::open(path).unwrap()) as Arc<dyn Vfs>
    };

    conformance::run_all(&mut fresh_root);
    let source = fresh_root();
    let target = fresh_root();
    let trash = base.join(format!("trash-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&trash);
    run_pipeline_smoke("exfat", &source, &target, Some(trash.clone()));

    let capabilities = LocalVfs::open(base.clone()).unwrap().caps();
    assert_eq!(capabilities.unix_mode, Support::No);
    #[cfg(target_os = "macos")]
    assert_eq!(capabilities.symlink, Support::Yes);
    assert_eq!(capabilities.file_id, Support::No);

    let probe = base.join(format!("file-id-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&probe);
    std::fs::create_dir_all(&probe).unwrap();
    std::fs::write(probe.join("empty.bin"), b"").unwrap();
    let snapshot = crate::pipeline::scan::scan(
        &probe,
        &crate::pipeline::scan::ScanOptions {
            hash: false,
            sampled: false,
            use_cache: false,
            symlinks_direct: false,
            filter: crate::pipeline::filter::PathFilter::build(&[], &[]),
        },
    )
    .unwrap();
    assert!(snapshot
        .entries
        .iter()
        .filter_map(crate::model::table::ObservedEntry::as_file)
        .all(|file| file.file_system_id.is_none()));

    let _ = std::fs::remove_dir_all(probe);
    let _ = std::fs::remove_dir_all(trash);
    for directory in directories {
        let _ = std::fs::remove_dir_all(directory);
    }
}
