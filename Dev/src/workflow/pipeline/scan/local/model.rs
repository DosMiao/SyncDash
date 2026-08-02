//! Local-scan working data passed from discovery to content evidence collection.

use crate::foundation::path::RootRelativePath;
use crate::model::digest::Blake3Digest;
use crate::model::table::{FileIdentityObservation, ObservedEntry, ObservedFile};

pub(super) struct PendingFile {
    pub relative: RootRelativePath,
    pub size: u64,
    pub raw_mtime_ms: i64,
    pub mtime_ms: i64,
    pub hash: Option<Blake3Digest>,
    /// Distinguishes failed evidence collection from a scan that did not request content evidence.
    pub hash_failed: bool,
    pub observed_file_id: Option<String>,
    pub file_id: Option<String>,
    pub mode: Option<u32>,
}

impl PendingFile {
    pub(super) fn into_entry(self, sampled: bool) -> ObservedEntry {
        let identity = if self.hash_failed {
            FileIdentityObservation::Unreadable
        } else if let Some(digest) = self.hash {
            if sampled && self.size >= super::super::digest::SAMPLE_MIN {
                FileIdentityObservation::SampledBlake3 { digest }
            } else {
                FileIdentityObservation::FullBlake3 { digest }
            }
        } else {
            FileIdentityObservation::SizeAndMtime
        };
        ObservedEntry::File(ObservedFile {
            path: self.relative,
            size: self.size,
            mtime_ms: self.mtime_ms,
            identity,
            file_system_id: self.file_id,
            mode: self.mode,
            previous_identities: Vec::new(),
        })
    }
}
