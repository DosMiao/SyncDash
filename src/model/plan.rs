//! L0 the plan: what a comparison decided, and the JSONL format it is written in.
//!
//! Kept out of `pipeline::compare` because it is vocabulary, not policy. `store::version` and
//! `obs::runlog` both persist ops, so with the types living in the compare engine those two
//! service-layer modules had to reach up into the domain layer for a struct definition. The
//! engine that *produces* a plan is one consumer of this format among several.
//!
//! `CompareOptions` and `ConflictPolicy` deliberately stay with the engine: they are knobs on
//! how a comparison is made, not part of the artifact it emits.

use serde::{Deserialize, Serialize};

pub const MTIME_SLACK_MS: i64 = 2000;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Copy,
    Update,
    Move,
    Delete,
    DeleteDir,
    /// Only the permission bits differ: don't retransmit the content, just write the mode back
    /// (syncthing's shortcutFile, `lib/model/folder_sendrecv.go:1253`)
    Chmod,
    Conflict,
    Note,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Source,
    Target,
}

#[derive(Serialize, Deserialize, Clone, Debug, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub struct Op {
    pub side: Side,
    pub action: Action,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(type = "number | null")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(type = "number | null")]
    pub mtime_ms: Option<i64>,
    /// Expected hash of the copied/updated content (used by paranoid mode to verify after copying)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub hash: Option<String>,
    /// Some = this is a symlink op, the value is the link target (apply creates a link instead of copying content)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub link: Option<String>,
    /// Target unix permission bits. Chmod uses it as the value to write; when Copy/Update carries it, it is written back right after the copy
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub mode: Option<u32>,
    pub reason: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub struct PlanHeader {
    pub schema: u32,
    pub kind: String,
    pub mode: String,
    #[ts(type = "number")]
    pub generated_at_ms: u64,
    pub source_root: String,
    pub source_host: String,
    pub target_root: String,
    pub target_host: String,
    #[ts(type = "number")]
    pub op_count: u64,
    #[ts(type = "number")]
    pub conflict_count: u64,
    /// Entry counts of both snapshots. Needed by the plan health check (deletion ratio), and lets the plan file attest to its own scale.
    #[serde(default)]
    #[ts(type = "number")]
    pub source_entries: u64,
    #[serde(default)]
    #[ts(type = "number")]
    pub target_entries: u64,
    /// Entries excluded by the filter on both sides (dirs + files). Exclusion must be visible: the UI uses this to spell out
    /// "how much did not take part in the compare" — never a swallowed tree hiding behind "both sides match ✓".
    #[serde(default)]
    #[ts(type = "number")]
    pub source_excluded: u64,
    #[serde(default)]
    #[ts(type = "number")]
    pub target_excluded: u64,
}

pub struct Plan {
    pub header: PlanHeader,
    pub ops: Vec<Op>,
}

impl Plan {
    pub fn write_to(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(w, "{}", serde_json::to_string(&self.header)?)?;
        for op in &self.ops {
            writeln!(w, "{}", serde_json::to_string(op)?)?;
        }
        Ok(())
    }
    pub fn load(path: &std::path::Path) -> std::io::Result<Plan> {
        use std::io::BufRead;
        let f = std::fs::File::open(path)?;
        let mut lines = std::io::BufReader::new(f).lines();
        let head = lines.next().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "empty plan"))??;
        let header: PlanHeader = serde_json::from_str(&head)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad plan header: {e}")))?;
        let mut ops = Vec::new();
        for line in lines {
            let line = line?;
            if line.trim().is_empty() { continue; }
            ops.push(serde_json::from_str(&line)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad op: {e}")))?);
        }
        Ok(Plan { header, ops })
    }
}
