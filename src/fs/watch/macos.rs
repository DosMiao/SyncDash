//! macOS FSEvents trigger backend.
//!
//! Each root gets a per-device stream so its event IDs are interpreted together with the UUID of
//! that volume's journal. Streams are started before [`watch_pair`] returns; callers can therefore
//! begin their bootstrap scan only after both subscriptions are live, as Apple requires. File-level
//! events stay disabled because paths are advisory triggers and the current scanner always verifies
//! the complete tree.

use std::ffi::{c_void, CStr, CString, OsStr};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::slice;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2_core_foundation::{CFAbsoluteTimeGetCurrent, CFArray, CFRetained, CFString, CFUUID};
use objc2_core_services::{
    kFSEventStreamCreateFlagWatchRoot, kFSEventStreamEventFlagEventIdsWrapped,
    kFSEventStreamEventFlagHistoryDone, kFSEventStreamEventFlagKernelDropped,
    kFSEventStreamEventFlagMustScanSubDirs, kFSEventStreamEventFlagRootChanged,
    kFSEventStreamEventFlagUnmount, kFSEventStreamEventFlagUserDropped,
    kFSEventStreamEventIdSinceNow, ConstFSEventStreamRef, FSEventStreamContext,
    FSEventStreamCreateRelativeToDevice, FSEventStreamEventFlags, FSEventStreamEventId,
    FSEventStreamInvalidate, FSEventStreamRef, FSEventStreamRelease, FSEventStreamSetDispatchQueue,
    FSEventStreamStart, FSEventStreamStop, FSEventsCopyUUIDForDevice,
    FSEventsGetLastEventIdForDeviceBeforeTime,
};

use super::{
    watch_channel, ArmedWatch, EventReducer, InvalidationKind, RawEvent, RawFlags, RootCursor,
    StreamSeed, WatchInvalidation, WatchMessage, WatchPosition, WatchReceiver, SOURCE_STREAM,
    TARGET_STREAM,
};

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub latency: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            latency: Duration::from_millis(500),
        }
    }
}

/// Two live FSEvents streams and their blocking trigger receiver.
pub struct MacWatcher {
    armed: ArmedWatch,
    receiver: WatchReceiver,
    reducer: Arc<Mutex<EventReducer>>,
    queue: DispatchRetained<DispatchQueue>,
    streams: Vec<NativeStream>,
}

impl MacWatcher {
    pub fn armed(&self) -> &ArmedWatch {
        &self.armed
    }

    pub fn receiver(&self) -> &WatchReceiver {
        &self.receiver
    }

    pub fn current_position(&self) -> std::io::Result<WatchPosition> {
        self.reducer
            .lock()
            .map(|reducer| reducer.position())
            .map_err(|_| std::io::Error::other("FSEvents cursor reducer is poisoned"))
    }
}

impl Drop for MacWatcher {
    fn drop(&mut self) {
        shutdown_streams(&self.queue, &mut self.streams);
    }
}

/// Arm source and target with the default FSEvents latency.
pub fn watch_pair(
    source: &Path,
    target: &Path,
    resume: Option<&WatchPosition>,
) -> std::io::Result<MacWatcher> {
    watch_pair_with_config(source, target, resume, Config::default())
}

/// Arm source and target before returning a handle.
pub fn watch_pair_with_config(
    source: &Path,
    target: &Path,
    resume: Option<&WatchPosition>,
    config: Config,
) -> std::io::Result<MacWatcher> {
    let prepared = [
        prepare_root(SOURCE_STREAM, source, resume)?,
        prepare_root(TARGET_STREAM, target, resume)?,
    ];
    let reducer = Arc::new(Mutex::new(
        EventReducer::new(prepared.iter().map(|root| root.seed.clone()))
            .map_err(|reason| std::io::Error::new(std::io::ErrorKind::InvalidInput, reason))?,
    ));
    let (sender, receiver) = watch_channel();
    let queue = DispatchQueue::new("com.syncdash.fsevents-trigger", DispatchQueueAttr::SERIAL);
    let mut streams = Vec::with_capacity(prepared.len());

    for root in &prepared {
        let context = Box::new(CallbackContext {
            stream: root.seed.stream.clone(),
            reducer: Arc::clone(&reducer),
            sender: sender.clone(),
        });
        let native_path = if root.seed.device_relative_root.is_empty() {
            "."
        } else {
            &root.seed.device_relative_root
        };
        let path = CFString::from_str(native_path);
        let paths = CFArray::from_retained_objects(&[path]);
        let mut native_context = FSEventStreamContext {
            version: 0,
            info: (&*context as *const CallbackContext).cast_mut().cast(),
            retain: None,
            release: None,
            copyDescription: None,
        };
        let raw = unsafe {
            FSEventStreamCreateRelativeToDevice(
                None,
                Some(fsevents_callback),
                &mut native_context,
                root.device,
                paths.as_opaque(),
                root.since_when,
                config.latency.as_secs_f64(),
                kFSEventStreamCreateFlagWatchRoot,
            )
        };
        if raw.is_null() {
            shutdown_streams(&queue, &mut streams);
            return Err(std::io::Error::other(format!(
                "FSEvents could not create the {:?} stream for {}",
                root.seed.stream,
                root.canonical_root.display()
            )));
        }
        unsafe { FSEventStreamSetDispatchQueue(raw, Some(&queue)) };
        streams.push(NativeStream {
            raw,
            started: false,
            _context: context,
            _paths: paths,
        });
        if !unsafe { FSEventStreamStart(raw) } {
            shutdown_streams(&queue, &mut streams);
            return Err(std::io::Error::other(format!(
                "FSEvents could not start the {:?} stream for {}",
                root.seed.stream,
                root.canonical_root.display()
            )));
        }
        streams
            .last_mut()
            .expect("the native stream was just pushed")
            .started = true;
    }

    let position = reducer
        .lock()
        .map_err(|_| std::io::Error::other("FSEvents cursor reducer is poisoned during startup"))?
        .position();
    let invalidations = prepared
        .into_iter()
        .flat_map(|root| root.invalidations)
        .collect();
    Ok(MacWatcher {
        armed: ArmedWatch {
            position,
            invalidations,
        },
        receiver,
        reducer,
        queue,
        streams,
    })
}

struct PreparedRoot {
    canonical_root: PathBuf,
    device: libc::dev_t,
    since_when: u64,
    seed: StreamSeed,
    invalidations: Vec<WatchInvalidation>,
}

fn prepare_root(
    stream: &str,
    root: &Path,
    resume: Option<&WatchPosition>,
) -> std::io::Result<PreparedRoot> {
    let canonical_root = std::fs::canonicalize(root)?;
    if !canonical_root.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "watch root is not a directory: {}",
                canonical_root.display()
            ),
        ));
    }
    let metadata = std::fs::metadata(&canonical_root)?;
    let device: libc::dev_t = metadata.dev().try_into().map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "device ID does not fit dev_t for {}",
                canonical_root.display()
            ),
        )
    })?;
    let mount_point = mount_point(&canonical_root)?;
    let device_relative_root = device_relative_root(&canonical_root, &mount_point)?;
    let journal_uuid = journal_uuid(device);
    let previous = resume.and_then(|position| position.streams.get(stream));
    let mut invalidations = Vec::new();

    let (since_when, cursor) = if let Some(uuid) = journal_uuid {
        let epoch = format!("fsevents:{uuid}");
        let latest = unsafe {
            FSEventsGetLastEventIdForDeviceBeforeTime(device, CFAbsoluteTimeGetCurrent())
        };
        match previous {
            Some(previous)
                if previous.journal_uuid.as_deref() == Some(uuid.as_str())
                    && previous.epoch == epoch
                    && previous.event_id <= latest =>
            {
                (previous.event_id, previous.clone())
            }
            Some(previous)
                if previous.journal_uuid.as_deref() == Some(uuid.as_str())
                    && previous.epoch == epoch =>
            {
                invalidations.push(WatchInvalidation {
                    stream: stream.into(),
                    kind: InvalidationKind::ResumeAheadOfJournal,
                });
                (
                    latest,
                    RootCursor {
                        journal_uuid: Some(uuid),
                        epoch,
                        event_id: latest,
                    },
                )
            }
            Some(_) => {
                invalidations.push(WatchInvalidation {
                    stream: stream.into(),
                    kind: InvalidationKind::JournalChanged,
                });
                (
                    latest,
                    RootCursor {
                        journal_uuid: Some(uuid),
                        epoch,
                        event_id: latest,
                    },
                )
            }
            None => (
                latest,
                RootCursor {
                    journal_uuid: Some(uuid),
                    epoch,
                    event_id: latest,
                },
            ),
        }
    } else {
        invalidations.push(WatchInvalidation {
            stream: stream.into(),
            kind: InvalidationKind::HistoryUnavailable,
        });
        let epoch = volatile_epoch(device)?;
        (
            kFSEventStreamEventIdSinceNow,
            RootCursor {
                journal_uuid: None,
                epoch,
                event_id: 0,
            },
        )
    };

    Ok(PreparedRoot {
        canonical_root,
        device,
        since_when,
        seed: StreamSeed {
            stream: stream.into(),
            device_relative_root,
            cursor,
        },
        invalidations,
    })
}

fn journal_uuid(device: libc::dev_t) -> Option<String> {
    let uuid = unsafe { FSEventsCopyUUIDForDevice(device) }?;
    CFUUID::new_string(None, Some(&uuid)).map(|value| value.to_string().to_ascii_lowercase())
}

fn volatile_epoch(device: libc::dev_t) -> std::io::Result<String> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| {
            std::io::Error::other(format!("system clock predates Unix epoch: {error}"))
        })?
        .as_nanos();
    let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(format!(
        "fsevents:volatile:{device}:{}:{nanos}:{sequence}",
        std::process::id()
    ))
}

fn mount_point(root: &Path) -> std::io::Result<PathBuf> {
    let root = CString::new(root.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("watch root contains a NUL byte: {}", root.display()),
        )
    })?;
    let mut info = std::mem::MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::statfs(root.as_ptr(), info.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let info = unsafe { info.assume_init() };
    let bytes = unsafe { CStr::from_ptr(info.f_mntonname.as_ptr()) }.to_bytes();
    Ok(PathBuf::from(OsStr::from_bytes(bytes)))
}

fn device_relative_root(root: &Path, mount_point: &Path) -> std::io::Result<String> {
    let relative = root
        .strip_prefix(mount_point)
        .ok()
        .or_else(|| root.strip_prefix(Path::new("/")).ok())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "cannot express {} relative to volume mount {}",
                    root.display(),
                    mount_point.display()
                ),
            )
        })?;
    relative
        .to_str()
        .map(|path| path.trim_matches('/').to_string())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("FSEvents root is not valid UTF-8: {}", root.display()),
            )
        })
}

struct CallbackContext {
    stream: String,
    reducer: Arc<Mutex<EventReducer>>,
    sender: std::sync::mpsc::Sender<WatchMessage>,
}

struct NativeStream {
    raw: FSEventStreamRef,
    started: bool,
    _context: Box<CallbackContext>,
    _paths: CFRetained<CFArray<CFString>>,
}

fn shutdown_streams(queue: &DispatchQueue, streams: &mut Vec<NativeStream>) {
    for stream in streams.iter_mut() {
        unsafe {
            if stream.started {
                FSEventStreamStop(stream.raw);
                stream.started = false;
            }
            FSEventStreamInvalidate(stream.raw);
        }
    }
    // The queue is serial. Draining it after invalidation guarantees no callback still references
    // a context when the boxes below are released.
    queue.exec_sync(|| {});
    for stream in streams.drain(..) {
        unsafe { FSEventStreamRelease(stream.raw) };
    }
}

unsafe extern "C-unwind" fn fsevents_callback(
    _stream: ConstFSEventStreamRef,
    info: *mut c_void,
    event_count: usize,
    event_paths: NonNull<c_void>,
    event_flags: NonNull<FSEventStreamEventFlags>,
    event_ids: NonNull<FSEventStreamEventId>,
) {
    let Some(context) = (unsafe { (info as *const CallbackContext).as_ref() }) else {
        return;
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        callback_inner(context, event_count, event_paths, event_flags, event_ids)
    }));
    if result.is_err() {
        let _ = context.sender.send(WatchMessage::BackendError {
            stream: Some(context.stream.clone()),
            message: "FSEvents callback panicked; watcher state is no longer trustworthy".into(),
        });
    }
}

unsafe fn callback_inner(
    context: &CallbackContext,
    event_count: usize,
    event_paths: NonNull<c_void>,
    event_flags: NonNull<FSEventStreamEventFlags>,
    event_ids: NonNull<FSEventStreamEventId>,
) {
    let paths = unsafe {
        slice::from_raw_parts(
            event_paths.as_ptr().cast::<*const std::ffi::c_char>(),
            event_count,
        )
    };
    let flags = unsafe { slice::from_raw_parts(event_flags.as_ptr(), event_count) };
    let ids = unsafe { slice::from_raw_parts(event_ids.as_ptr(), event_count) };
    let mut events = Vec::with_capacity(event_count);
    for ((path, flags), event_id) in paths.iter().zip(flags).zip(ids) {
        if path.is_null() {
            send_backend_error(context, "FSEvents returned a null event path");
            return;
        }
        let path = match unsafe { CStr::from_ptr(*path) }.to_str() {
            Ok(path) => path.to_string(),
            Err(_) => {
                send_backend_error(context, "FSEvents returned a non-UTF-8 event path");
                return;
            }
        };
        events.push(RawEvent {
            event_id: *event_id,
            device_relative_path: path,
            flags: raw_flags(*flags),
        });
    }

    let mut reducer = match context.reducer.lock() {
        Ok(reducer) => reducer,
        Err(_) => {
            send_backend_error(context, "FSEvents cursor reducer is poisoned");
            return;
        }
    };
    match reducer.reduce(&context.stream, events) {
        Ok(Some(batch)) => {
            let _ = context.sender.send(WatchMessage::Trigger(batch));
        }
        Ok(None) => {}
        Err(reason) => send_backend_error(context, &reason),
    }
}

fn send_backend_error(context: &CallbackContext, message: &str) {
    let _ = context.sender.send(WatchMessage::BackendError {
        stream: Some(context.stream.clone()),
        message: message.into(),
    });
}

fn raw_flags(flags: FSEventStreamEventFlags) -> RawFlags {
    let has = |mask| flags & mask != 0;
    RawFlags {
        must_scan_subdirectories: has(kFSEventStreamEventFlagMustScanSubDirs),
        user_dropped: has(kFSEventStreamEventFlagUserDropped),
        kernel_dropped: has(kFSEventStreamEventFlagKernelDropped),
        event_ids_wrapped: has(kFSEventStreamEventFlagEventIdsWrapped),
        history_done: has(kFSEventStreamEventFlagHistoryDone),
        root_changed: has(kFSEventStreamEventFlagRootChanged),
        unmounted: has(kFSEventStreamEventFlagUnmount),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_relative_paths_have_no_system_mount_prefix() {
        let root = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let mount = mount_point(&root).unwrap();
        let relative = device_relative_root(&root, &mount).unwrap();
        assert!(!relative.starts_with('/'));
        assert!(!relative.contains(".."));
    }

    #[test]
    fn pair_is_armed_before_a_post_start_change_is_received() {
        let base = std::env::temp_dir().join(format!(
            "syncdash-fsevents-pair-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let source = base.join("source");
        let target = base.join("target");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&target).unwrap();

        let watcher = watch_pair_with_config(
            &source,
            &target,
            None,
            Config {
                latency: Duration::from_millis(50),
            },
        )
        .unwrap();
        assert!(watcher.armed().position.streams.contains_key(SOURCE_STREAM));
        assert!(watcher.armed().position.streams.contains_key(TARGET_STREAM));

        std::fs::write(source.join("after-arm.txt"), b"trigger").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut saw_source = false;
        while std::time::Instant::now() < deadline {
            match watcher.receiver().recv_timeout(Duration::from_millis(500)) {
                Ok(WatchMessage::Trigger(batch)) => {
                    if batch
                        .changed_paths
                        .iter()
                        .any(|path| path.stream == SOURCE_STREAM)
                        || batch
                            .invalidations
                            .iter()
                            .any(|invalidation| invalidation.stream == SOURCE_STREAM)
                    {
                        saw_source = true;
                        break;
                    }
                }
                Ok(WatchMessage::BackendError { message, .. }) => panic!("{message}"),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(error) => panic!("FSEvents channel failed: {error}"),
            }
        }
        drop(watcher);
        let _ = std::fs::remove_dir_all(&base);
        assert!(
            saw_source,
            "a change made after watch_pair returned must be delivered"
        );
    }
}
