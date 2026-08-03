//! Working data both scan lanes carry from discovery to content evidence collection.
//!
//! One model rather than two: the lanes reach bytes differently, but the row they are building is
//! the same row, and `into_entry` is the single rule that turns collected evidence into a
//! `FileIdentityObservation`. A second copy of that rule is how a table starts claiming a full
//! hash for a sampled read.

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
    /// Local lane only: the identity the walk observed, which `local::hashing` re-checks after the
    /// read so a file replaced underneath the reader cannot pass its bytes off as this row's. The
    /// generic lane leaves it `None` and re-stats through the backend instead, because every
    /// protocol backend declares `file_id: Support::No` and would have nothing to compare.
    pub observed_file_id: Option<String>,
    pub file_id: Option<String>,
    pub mode: Option<u32>,
}

impl PendingFile {
    pub(super) fn into_entry(self, sampled: bool) -> ObservedEntry {
        let identity = if self.hash_failed {
            FileIdentityObservation::Unreadable
        } else if let Some(digest) = self.hash {
            if sampled && self.size >= super::digest::SAMPLE_MIN {
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
