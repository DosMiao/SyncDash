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

/// The plan file format's own version, distinct from `table::SCHEMA` — a plan and a snapshot are
/// different artifacts and have no reason to move together. The header carried the table's number
/// for want of one of its own, and nothing read it back, so the field was decoration.
///
/// **2**: `Op` gained fields that a reader cannot invent. Bumped rather than defaulted because a
/// plan is *derived* data — re-running Compare reproduces it — so refusing an older one costs a
/// keystroke, while quietly defaulting a missing field would let a plan execute under an assumption
/// its author never made. A job file is the opposite case, and gets a real migration.
pub const PLAN_SCHEMA: u32 = 2;

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
    /// Entries the walk could not read on each side. The sibling of `*_excluded` and the more
    /// dangerous one: an exclusion is a choice, this is a tree the scan could not see, and compare
    /// cannot tell the two apart from the entries alone — both read as "absent on this side".
    /// Carried onto the plan because preflight runs against the plan long after the snapshots are
    /// gone, and a plan that cannot say its scan was incomplete cannot be judged.
    #[serde(default)]
    #[ts(type = "number")]
    pub source_walk_errors: u64,
    #[serde(default)]
    #[ts(type = "number")]
    pub target_walk_errors: u64,
    /// Sampled failures, so the refusal names what could not be read rather than only counting.
    #[serde(default)]
    pub source_walk_err_samples: Vec<String>,
    #[serde(default)]
    pub target_walk_err_samples: Vec<String>,
    /// iCloud placeholders seen on each side. Carried for the same reason as the walk errors: the
    /// gate runs against the plan, and by then the snapshots are gone.
    #[serde(default)]
    #[ts(type = "number")]
    pub source_icloud_stubs: u64,
    #[serde(default)]
    #[ts(type = "number")]
    pub target_icloud_stubs: u64,
    #[serde(default)]
    pub source_icloud_stub_samples: Vec<String>,
    #[serde(default)]
    pub target_icloud_stub_samples: Vec<String>,
}

pub struct Plan {
    pub header: PlanHeader,
    pub ops: Vec<Op>,
}

impl Plan {
    /// Content identity of the original comparison plan.
    ///
    /// `write_to` is the canonical public plan encoding: header first, then one op per line, with
    /// struct field order fixed by the Rust schema. Hashing those bytes catches any header or op
    /// mutation while deliberately excluding desktop-only evidence/view metadata.
    pub fn digest(&self) -> String {
        let mut bytes = Vec::new();
        self.write_to(&mut bytes)
            .expect("serializing a Plan into memory cannot fail");
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"syncdash-plan-v1\0");
        hasher.update(&bytes);
        hasher.finalize().to_hex().to_string()
    }

    pub fn write_to(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(w, "{}", serde_json::to_string(&self.header)?)?;
        for op in &self.ops {
            writeln!(w, "{}", serde_json::to_string(op)?)?;
        }
        Ok(())
    }
    pub fn load(path: &std::path::Path) -> std::io::Result<Plan> {
        Plan::from_reader(std::io::BufReader::new(std::fs::File::open(path)?))
    }
    /// Parse from any reader. `apply_pack` holds the plan as bytes already, and routing it
    /// through a temp file to reach `load` gave two concurrent calls the same path to fight over.
    pub fn from_reader(r: impl std::io::BufRead) -> std::io::Result<Plan> {
        let mut lines = r.lines();
        let head = lines.next().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "empty plan"))??;
        let header: PlanHeader = serde_json::from_str(&head)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad plan header: {e}")))?;
        // Read the version before the ops, so an older plan is named as an older plan instead of
        // failing later as "bad op: missing field ..." — the same bytes, but one of those messages
        // reads as corruption and sends the user looking for a damaged file.
        if header.schema != PLAN_SCHEMA {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "this plan is format version {} and this build writes {PLAN_SCHEMA} — re-run Compare to produce a current one \
                     (a plan is derived from the two roots, so nothing is lost by regenerating it)",
                    header.schema
                ),
            ));
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A plan written by an older build must be named as such, not decoded as far as its first
    /// missing field. Both messages come from the same bytes, but "bad op: missing field" reads as
    /// corruption and sends the user looking for a damaged file.
    #[test]
    fn an_older_plan_is_refused_by_version_not_by_a_parse_error() {
        let mut buf = Vec::new();
        let mut p = Plan { header: header(1), ops: vec![] };
        p.header.schema = PLAN_SCHEMA - 1;
        p.write_to(&mut buf).unwrap();
        let e = match Plan::from_reader(std::io::BufReader::new(&buf[..])) {
            Err(e) => e,
            Ok(_) => panic!("a plan from an older format must not load"),
        };
        let msg = e.to_string();
        assert!(msg.contains("format version"), "{msg}");
        assert!(msg.contains("re-run Compare"), "the message has to carry the remedy: {msg}");
    }

    fn header(ops: usize) -> PlanHeader {
        PlanHeader {
            schema: PLAN_SCHEMA,
            kind: "plan".into(),
            mode: "mirror".into(),
            generated_at_ms: 1_700_000_000_000,
            source_root: r"D:\src".into(),
            source_host: "win01".into(),
            target_root: "/Volumes/tgt".into(),
            target_host: "mac".into(),
            op_count: ops as u64,
            conflict_count: 1,
            source_entries: 10,
            target_entries: 9,
            source_excluded: 2,
            target_excluded: 3,
            source_walk_errors: 0,
            target_walk_errors: 0,
            source_walk_err_samples: Vec::new(),
            target_walk_err_samples: Vec::new(),
            source_icloud_stubs: 0,
            target_icloud_stubs: 0,
            source_icloud_stub_samples: Vec::new(),
            target_icloud_stub_samples: Vec::new(),
        }
    }

    fn op(action: Action, path: &str) -> Op {
        Op {
            side: Side::Target,
            action,
            path: path.into(),
            from: None,
            size: Some(4096),
            mtime_ms: Some(1_700_000_000_000),
            hash: Some("deadbeef".into()),
            link: None,
            mode: Some(0o644),
            reason: "because".into(),
        }
    }

    /// The plan crosses machines inside a package and is replayed on the far side, so what
    /// survives serialization is exactly what the remote will execute.
    #[test]
    fn a_plan_survives_write_then_read() {
        let plan = Plan {
            header: header(2),
            ops: vec![op(Action::Copy, "a/one.txt"), Op { from: Some("old/two.bin".into()), ..op(Action::Move, "new/two.bin") }],
        };
        let mut buf: Vec<u8> = Vec::new();
        plan.write_to(&mut buf).unwrap();
        let back = Plan::from_reader(std::io::BufReader::new(&buf[..])).unwrap();

        assert_eq!(back.header.mode, "mirror");
        assert_eq!(back.header.target_root, "/Volumes/tgt");
        assert_eq!(back.header.conflict_count, 1);
        assert_eq!(back.ops.len(), 2);
        assert_eq!(back.ops[0].action, Action::Copy);
        assert_eq!(back.ops[0].mode, Some(0o644), "the mode SMB cannot carry must survive the plan");
        assert_eq!(back.ops[1].action, Action::Move);
        assert_eq!(back.ops[1].from.as_deref(), Some("old/two.bin"), "a move without its origin is a copy");
    }

    #[test]
    fn digest_identifies_the_complete_serialized_plan() {
        let plan = Plan { header: header(1), ops: vec![op(Action::Copy, "a/one.txt")] };
        let revision = plan.digest();
        assert_eq!(revision.len(), 64);

        let mut bytes = Vec::new();
        plan.write_to(&mut bytes).unwrap();
        let round_trip = Plan::from_reader(std::io::BufReader::new(&bytes[..])).unwrap();
        assert_eq!(revision, round_trip.digest(), "the public JSONL encoding is canonical");

        let mut changed = round_trip;
        changed.ops[0].reason = "different evidence".into();
        assert_ne!(revision, changed.digest(), "any executable plan mutation must be detected");
    }

    /// Action and Side serialize snake_case, and the CSV export, the event stream and the plan
    /// JSONL all quote that same spelling. Debug's PascalCase would put them out of step.
    #[test]
    fn action_and_side_spell_themselves_snake_case() {
        let j = serde_json::to_string(&op(Action::DeleteDir, "d")).unwrap();
        assert!(j.contains("\"delete_dir\""), "{j}");
        assert!(j.contains("\"target\""), "{j}");
    }

    /// An empty file is not a valid plan — better a loud parse error than an empty op list
    /// silently reported as "nothing to do".
    #[test]
    fn an_empty_plan_file_is_an_error_not_an_empty_plan() {
        let e = match Plan::from_reader(std::io::BufReader::new(&b""[..])) {
            Err(e) => e,
            Ok(_) => panic!("an empty file must not parse as a plan"),
        };
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
    }
}
