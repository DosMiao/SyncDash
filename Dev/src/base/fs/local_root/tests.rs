use super::*;
use std::io::Write;

fn test_directory(name: &str) -> PathBuf {
    let directory =
        std::env::temp_dir().join(format!("syncdash-local-root-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

fn path(value: &str) -> RootRelativePath {
    RootRelativePath::try_from(value).unwrap()
}

fn directory(value: &str) -> RootRelativeDir {
    RootRelativeDir::try_from(value).unwrap()
}

#[test]
fn traversal_never_reaches_the_capability_api() {
    assert!(RootRelativePath::try_from("../outside").is_err());
    assert!(RootRelativePath::try_from("safe/../../outside").is_err());
    assert!(RootRelativePath::try_from("/outside").is_err());
    assert!(RootRelativePath::try_from(r"C:\outside").is_err());
}

#[test]
fn lock_file_is_created_with_read_and_write_access() {
    let root_path = test_directory("lock-file");
    let root = LocalRoot::open(root_path.clone()).unwrap();
    let lock_path = path("mutation.lock");
    let mut file = root.open_lock_file(&lock_path).unwrap();

    file.write_all(b"owner").unwrap();
    file.seek(std::io::SeekFrom::Start(0)).unwrap();
    let mut owner = String::new();
    file.read_to_string(&mut owner).unwrap();
    assert_eq!(owner, "owner");
    let second = root.open_lock_file(&lock_path).unwrap();
    assert!(second.metadata().unwrap().is_file());
    file.lock().unwrap();
    assert!(matches!(
        second.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    file.unlock().unwrap();

    let _ = std::fs::remove_dir_all(root_path);
}

#[test]
fn new_regular_file_creation_is_exclusive_and_read_write() {
    let root_path = test_directory("new-regular-file");
    let root = LocalRoot::open(root_path.clone()).unwrap();
    let relative = path("package.tmp");
    let mut file = root.create_regular_file_new(&relative).unwrap();

    file.write_all(b"package").unwrap();
    file.seek(std::io::SeekFrom::Start(0)).unwrap();
    let mut contents = String::new();
    file.read_to_string(&mut contents).unwrap();
    assert_eq!(contents, "package");
    assert_eq!(
        root.create_regular_file_new(&relative).unwrap_err().kind(),
        std::io::ErrorKind::AlreadyExists
    );

    let _ = std::fs::remove_dir_all(root_path);
}

#[cfg(unix)]
#[test]
fn lock_file_never_follows_a_symlink() {
    use std::os::unix::fs::symlink;

    let root_path = test_directory("lock-symlink-root");
    let outside = test_directory("lock-symlink-outside");
    let outside_file = outside.join("outside.lock");
    std::fs::write(&outside_file, b"outside").unwrap();
    symlink(&outside_file, root_path.join("mutation.lock")).unwrap();
    let root = LocalRoot::open(root_path.clone()).unwrap();

    assert!(root.open_lock_file(&path("mutation.lock")).is_err());
    assert_eq!(std::fs::read(&outside_file).unwrap(), b"outside");

    let _ = std::fs::remove_dir_all(root_path);
    let _ = std::fs::remove_dir_all(outside);
}

#[cfg(unix)]
#[test]
fn intermediate_and_final_symlinks_are_never_followed() {
    use std::os::unix::fs::symlink;

    let root_path = test_directory("symlink-refusal-root");
    let outside = test_directory("symlink-refusal-outside");
    std::fs::write(outside.join("sentinel"), b"outside").unwrap();
    symlink(&outside, root_path.join("redirect")).unwrap();
    symlink(outside.join("sentinel"), root_path.join("final-link")).unwrap();
    let root = LocalRoot::open(root_path.clone()).unwrap();

    assert!(root.read(&path("redirect/sentinel")).is_err());
    assert!(root.read(&path("final-link")).is_err());
    root.remove_file(&path("final-link")).unwrap();
    assert_eq!(std::fs::read(outside.join("sentinel")).unwrap(), b"outside");

    let _ = std::fs::remove_dir_all(root_path);
    let _ = std::fs::remove_dir_all(outside);
}

#[cfg(unix)]
#[test]
fn read_link_returns_an_absolute_target_without_following_it() {
    use std::os::unix::fs::symlink;

    let root_path = test_directory("absolute-link-root");
    let outside = root_path.with_file_name(format!(
        "syncdash-local-root-absolute-target-{}",
        std::process::id()
    ));
    std::fs::write(&outside, b"outside").unwrap();
    symlink(&outside, root_path.join("link")).unwrap();
    let root = LocalRoot::open(root_path.clone()).unwrap();

    assert_eq!(root.read_link(&path("link")).unwrap(), outside);
    assert!(root.read(&path("link")).is_err());

    let _ = std::fs::remove_file(outside);
    let _ = std::fs::remove_dir_all(root_path);
}

#[cfg(unix)]
#[test]
fn staged_commit_stays_with_the_parent_handle_after_name_substitution() {
    use std::os::unix::fs::symlink;

    let root_path = test_directory("staged-parent-root");
    let outside = test_directory("staged-parent-outside");
    std::fs::create_dir(root_path.join("parent")).unwrap();
    let root = LocalRoot::open(root_path.clone()).unwrap();
    let mut staged = root.create_staged(&path("parent/result")).unwrap();
    staged.write_all(b"confined").unwrap();

    std::fs::rename(root_path.join("parent"), root_path.join("detached")).unwrap();
    symlink(&outside, root_path.join("parent")).unwrap();
    staged.commit().unwrap();

    assert_eq!(
        std::fs::read(root_path.join("detached/result")).unwrap(),
        b"confined"
    );
    assert!(!outside.join("result").exists());
    let _ = std::fs::remove_dir_all(root_path);
    let _ = std::fs::remove_dir_all(outside);
}

#[cfg(unix)]
#[test]
fn direct_write_and_remove_refuse_an_intermediate_symlink_substitution() {
    use std::os::unix::fs::symlink;

    let root_path = test_directory("direct-parent-root");
    let outside = test_directory("direct-parent-outside");
    std::fs::create_dir(root_path.join("parent")).unwrap();
    std::fs::write(root_path.join("parent/victim"), b"inside").unwrap();
    std::fs::write(outside.join("victim"), b"outside").unwrap();
    let root = LocalRoot::open(root_path.clone()).unwrap();

    std::fs::rename(root_path.join("parent"), root_path.join("detached")).unwrap();
    symlink(&outside, root_path.join("parent")).unwrap();

    assert!(root.open_append(&path("parent/new")).is_err());
    assert!(root.remove_file(&path("parent/victim")).is_err());
    assert!(!outside.join("new").exists());
    assert_eq!(std::fs::read(outside.join("victim")).unwrap(), b"outside");
    assert_eq!(
        std::fs::read(root_path.join("detached/victim")).unwrap(),
        b"inside"
    );
    let _ = std::fs::remove_dir_all(root_path);
    let _ = std::fs::remove_dir_all(outside);
}

#[cfg(unix)]
#[test]
fn concurrent_parent_swaps_never_read_the_outside_sentinel() {
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicBool, Ordering};

    let root_path = test_directory("swap-root");
    let outside = test_directory("swap-outside");
    std::fs::create_dir(root_path.join("safe")).unwrap();
    std::fs::write(outside.join("sentinel"), b"outside-secret").unwrap();
    let root = LocalRoot::open(root_path.clone()).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let thread_root = root_path.clone();
    let thread_outside = outside.clone();
    let swapper = std::thread::spawn(move || {
        while !thread_stop.load(Ordering::Relaxed) {
            if std::fs::rename(thread_root.join("safe"), thread_root.join("held")).is_ok() {
                if symlink(&thread_outside, thread_root.join("safe")).is_ok() {
                    let _ = std::fs::remove_file(thread_root.join("safe"));
                }
                let _ = std::fs::rename(thread_root.join("held"), thread_root.join("safe"));
            }
        }
    });

    for _ in 0..2_000 {
        if let Ok(bytes) = root.read(&path("safe/sentinel")) {
            assert_ne!(bytes, b"outside-secret");
        }
    }
    stop.store(true, Ordering::Relaxed);
    swapper.join().unwrap();

    assert_eq!(
        std::fs::read(outside.join("sentinel")).unwrap(),
        b"outside-secret"
    );
    let _ = std::fs::remove_dir_all(root_path);
    let _ = std::fs::remove_dir_all(outside);
}

#[test]
fn no_replace_commit_keeps_an_existing_destination() {
    let root_path = test_directory("no-replace");
    std::fs::write(root_path.join("destination"), b"existing").unwrap();
    let root = LocalRoot::open(root_path.clone()).unwrap();
    let mut staged = root.create_staged(&path("destination")).unwrap();
    staged.write_all(b"replacement").unwrap();

    let error = staged.commit_noreplace().unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(
        std::fs::read(root_path.join("destination")).unwrap(),
        b"existing"
    );
    let _ = std::fs::remove_dir_all(root_path);
}

#[test]
fn identity_checked_removal_restores_a_different_current_entry() {
    let root_path = test_directory("identity-removal");
    std::fs::write(root_path.join("expected"), b"expected").unwrap();
    std::fs::write(root_path.join("current"), b"current").unwrap();
    let root = LocalRoot::open(root_path.clone()).unwrap();
    let expected = root.open_read(&path("expected")).unwrap();

    let error = root
        .remove_open_file(&path("current"), &expected)
        .unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert_eq!(
        std::fs::read(root_path.join("current")).unwrap(),
        b"current"
    );
    assert_eq!(
        std::fs::read(root_path.join("expected")).unwrap(),
        b"expected"
    );
    assert!(std::fs::read_dir(&root_path).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(TEMP_PREFIX)));
    drop(expected);
    let _ = std::fs::remove_dir_all(root_path);
}

#[test]
fn no_replace_rename_moves_a_directory_as_one_entry_operation() {
    let root_path = test_directory("no-replace-directory");
    std::fs::create_dir(root_path.join("source-directory")).unwrap();
    let root = LocalRoot::open(root_path.clone()).unwrap();

    root.rename_noreplace(&path("source-directory"), &path("destination-directory"))
        .unwrap();
    assert!(!root_path.join("source-directory").exists());
    assert!(root_path.join("destination-directory").is_dir());
    let _ = std::fs::remove_dir_all(root_path);
}

#[cfg(unix)]
#[test]
fn no_replace_rename_moves_a_symlink_without_following_it() {
    use std::os::unix::fs::symlink;

    let root_path = test_directory("no-replace-symlink");
    std::fs::write(root_path.join("target"), b"target").unwrap();
    symlink("target", root_path.join("source-link")).unwrap();
    let root = LocalRoot::open(root_path.clone()).unwrap();

    root.rename_noreplace(&path("source-link"), &path("destination-link"))
        .unwrap();
    assert!(!root_path.join("source-link").exists());
    assert_eq!(
        std::fs::read_link(root_path.join("destination-link")).unwrap(),
        PathBuf::from("target")
    );
    assert_eq!(std::fs::read(root_path.join("target")).unwrap(), b"target");
    let _ = std::fs::remove_dir_all(root_path);
}

#[cfg(unix)]
#[test]
fn recursive_remove_refuses_a_symlinked_directory() {
    use std::os::unix::fs::symlink;

    let root_path = test_directory("recursive-root");
    let outside = test_directory("recursive-outside");
    std::fs::write(outside.join("sentinel"), b"outside").unwrap();
    symlink(&outside, root_path.join("tree")).unwrap();
    let root = LocalRoot::open(root_path.clone()).unwrap();

    assert!(root.remove_directory_all(&directory("tree")).is_err());
    assert_eq!(std::fs::read(outside.join("sentinel")).unwrap(), b"outside");
    let _ = std::fs::remove_dir_all(root_path);
    let _ = std::fs::remove_dir_all(outside);
}
