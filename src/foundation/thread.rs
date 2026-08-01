//! Process-wide worker-thread policy shared by startup and bounded scan pools.

pub fn configured_worker_limit() -> Option<usize> {
    std::env::var("SYNCDASH_SCAN_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|threads| *threads >= 1)
        .map(|threads| threads.min(64))
}

#[cfg(windows)]
pub fn lower_priority() {
    extern "system" {
        fn GetCurrentThread() -> isize;
        fn SetThreadPriority(handle: isize, priority: i32) -> i32;
    }
    unsafe {
        SetThreadPriority(GetCurrentThread(), -1);
    }
}

#[cfg(target_os = "linux")]
pub fn lower_priority() {
    unsafe {
        libc::nice(3);
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
pub fn lower_priority() {}
