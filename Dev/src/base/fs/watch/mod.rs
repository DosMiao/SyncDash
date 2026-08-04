//! L0 filesystem change triggers.
//!
//! Watchers report that a tree may have changed; they never claim that an event list is a complete
//! snapshot.  The public cursor and invalidation vocabulary is platform-neutral, while `macos`
//! owns the FSEvents lifecycle and `reducer` owns the pure reduction of its raw records.

use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, RecvError, RecvTimeoutError, SendError, Sender, TryRecvError};
use std::time::Duration;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(any(target_os = "macos", test))]
mod reducer;

pub const SOURCE_STREAM: &str = "source";
pub const TARGET_STREAM: &str = "target";

/// Durable position for one watched root.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct RootCursor {
    /// The UUID stored in the volume's FSEvents journal, when the volume exposes one.
    pub journal_uuid: Option<String>,
    /// Stream generation. Normally derived from `journal_uuid`; rotated after an ID reset.
    pub epoch: String,
    pub event_id: u64,
}

/// One consistent cursor spanning every root participating in a compare.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct WatchPosition {
    pub streams: BTreeMap<String, RootCursor>,
}

impl WatchPosition {
    pub fn validate(&self) -> Result<(), String> {
        if self.streams.is_empty() {
            return Err("a watch position must contain at least one stream".into());
        }
        for (name, cursor) in &self.streams {
            if name.trim().is_empty() {
                return Err("a watch stream name cannot be empty".into());
            }
            if cursor.epoch.trim().is_empty() {
                return Err(format!("watch stream {name:?} has an empty epoch"));
            }
            if cursor
                .journal_uuid
                .as_ref()
                .is_some_and(|uuid| uuid.trim().is_empty())
            {
                return Err(format!("watch stream {name:?} has an empty journal UUID"));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum InvalidationKind {
    /// This volume has no persistent journal UUID; events cannot be replayed after a restart.
    HistoryUnavailable,
    /// The saved cursor belongs to a different journal generation.
    JournalChanged,
    /// The saved ID is newer than the journal currently reports.
    ResumeAheadOfJournal,
    /// FSEvents coalesced changes and requires a recursive rescan.
    MustScanSubdirectories,
    /// The client-side FSEvents queue dropped records.
    UserDropped,
    /// The kernel-to-fseventsd path dropped records.
    KernelDropped,
    /// Event IDs wrapped or otherwise moved backwards.
    EventIdsWrapped,
    /// The watched root or one of its parents was renamed, moved, or deleted.
    RootChanged,
    /// The watched volume was unmounted.
    Unmounted,
    /// The event names the watched root itself, not one safely addressable descendant.
    WholeRootChanged,
    /// FSEvents returned a path outside the root registered for this stream.
    PathOutsideRoot,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct WatchInvalidation {
    pub stream: String,
    pub kind: InvalidationKind,
}

/// Advisory path that caused a trigger, qualified by its source/target stream.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TriggerPath {
    pub stream: String,
    pub path: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TriggerBatch {
    pub position: WatchPosition,
    pub changed_paths: Vec<TriggerPath>,
    pub invalidations: Vec<WatchInvalidation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WatchMessage {
    Trigger(TriggerBatch),
    BackendError {
        stream: Option<String>,
        message: String,
    },
}

/// Cursor and fallback facts established only after every native stream has started.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArmedWatch {
    pub position: WatchPosition,
    pub invalidations: Vec<WatchInvalidation>,
}

/// Blocking receiver used by both the native backend and deterministic orchestration tests.
pub struct WatchReceiver {
    receiver: Receiver<WatchMessage>,
}

impl WatchReceiver {
    pub fn recv(&self) -> Result<WatchMessage, RecvError> {
        self.receiver.recv()
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<WatchMessage, RecvTimeoutError> {
        self.receiver.recv_timeout(timeout)
    }

    pub fn try_recv(&self) -> Result<WatchMessage, TryRecvError> {
        self.receiver.try_recv()
    }
}

/// Sender half of the pure scripted seam. It follows the same channel contract as FSEvents.
#[derive(Clone)]
pub struct ScriptedWatch {
    sender: Sender<WatchMessage>,
}

impl ScriptedWatch {
    pub fn send(&self, message: WatchMessage) -> Result<(), SendError<WatchMessage>> {
        self.sender.send(message)
    }
}

pub fn scripted_channel() -> (ScriptedWatch, WatchReceiver) {
    let (sender, receiver) = watch_channel();
    (ScriptedWatch { sender }, receiver)
}

pub(super) fn watch_channel() -> (Sender<WatchMessage>, WatchReceiver) {
    let (sender, receiver) = std::sync::mpsc::channel();
    (sender, WatchReceiver { receiver })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripted_channel_has_the_same_blocking_contract_as_the_native_backend() {
        let (script, receiver) = scripted_channel();
        script
            .send(WatchMessage::BackendError {
                stream: None,
                message: "scripted".into(),
            })
            .unwrap();
        assert_eq!(
            receiver.recv().unwrap(),
            WatchMessage::BackendError {
                stream: None,
                message: "scripted".into()
            }
        );
    }
}
