//! Emits the golden vectors that hold the desktop window's TypeScript copy of the compare-plan
//! rules to this engine.
//!
//! Reversal, per-side path derivation, elided-metadata reconstruction, and the action rank each
//! exist twice: once in Rust, and once in `Dev/typescript/core/domain/compare/plan.ts`, because the
//! window cannot ask the backend per click for a direction toggle or per keystroke for a six-figure
//! table. `evidence`'s module header records which of them is engine semantics and which is
//! presentation; Rust is the owner of all four either way.
//!
//! Nothing else in the repository compares the two implementations, and the channel between them is
//! silent: Apply sends `{index, direction_reversed}`, so a frontend that reversed a row differently
//! from `reverse_op` would show the operator one operation and execute another with no error
//! anywhere. These vectors close that by construction rather than by review.
//!
//! The emitted file lands in the ts-rs output directory, so it inherits that directory's staleness
//! gate: change a rule without running `npm run gen:types` and `npm run gen:types:check` fails,
//! change the TypeScript copy without matching a current file and
//! `Script/tests/compare-plan-rules.test.mts` fails.

use std::path::PathBuf;

use serde::Serialize;

use crate::model::plan::{Action, Op, Side};
use crate::pipeline::compare::evidence::{reverse_op, row_meta, side_paths, RowMeta, SideMeta};

/// One case, evaluated here and replayed against TypeScript.
#[derive(Serialize)]
struct RuleVector {
    name: String,
    op: Op,
    /// The row's retained per-side evidence, exactly as `PlanDto.metas[i]` carries it.
    meta: RowMeta,
    action_rank: u8,
    /// `null` where this engine refuses to reverse the row. The window must refuse it too rather
    /// than fall back to the forward operation, which points the opposite way.
    reversed: Option<Op>,
    side_paths: (Option<String>, Option<String>),
    /// Side paths of the reversed shape, absent when the row does not reverse.
    reversed_side_paths: Option<(Option<String>, Option<String>)>,
    /// What a reader has to rebuild when `PlanDto.metas[i]` is `null`.
    reconstructed_meta: RowMeta,
}

struct FieldProfile {
    name: &'static str,
    path: &'static str,
    from: Option<&'static str>,
    size: Option<u64>,
    mtime_ms: Option<i64>,
    hash: Option<&'static str>,
    link: Option<&'static str>,
    mode: Option<u32>,
}

const BARE: FieldProfile = FieldProfile {
    name: "bare",
    path: "top.txt",
    from: None,
    size: None,
    mtime_ms: None,
    hash: None,
    link: None,
    mode: None,
};

/// The field profiles a row can arrive in. `renamed` is the awkward one: an operation carries a
/// single `path`, so a row whose two sides are spelled differently has to derive both from it.
/// `folder` is the row the window shortens to "(this folder)" — the derivation it shortens is here,
/// the shortening itself is not.
const PROFILES: [FieldProfile; 7] = [
    BARE,
    FieldProfile {
        name: "measured",
        path: "docs/deep/report.txt",
        size: Some(42),
        mtime_ms: Some(1_700_000_000_000),
        hash: Some("b3:0123456789abcdef"),
        ..BARE
    },
    FieldProfile {
        name: "renamed",
        path: "docs/new name.txt",
        from: Some("docs/old name.txt"),
        size: Some(7),
        mtime_ms: Some(1_600_000_000_000),
        ..BARE
    },
    FieldProfile {
        name: "symlink",
        path: "links/alias",
        link: Some("../elsewhere/file.bin"),
        mode: Some(0o777),
        ..BARE
    },
    FieldProfile {
        name: "zeroed",
        path: "empty.bin",
        size: Some(0),
        mtime_ms: Some(0),
        ..BARE
    },
    FieldProfile {
        name: "mode_only",
        path: "scripts/run.sh",
        mode: Some(0o644),
        ..BARE
    },
    FieldProfile {
        name: "folder",
        path: "docs/deep",
        ..BARE
    },
];

fn measured(size: u64, mtime_ms: i64) -> Option<SideMeta> {
    Some(SideMeta { size, mtime_ms })
}

/// Both sides measured, one side measured, and neither: a reversed content Update reads the side it
/// is about to write *from*, so the missing-side cases are the ones that decide whether the row is
/// reversible at all.
fn metadata_cases() -> [(&'static str, RowMeta); 4] {
    [
        ("unmeasured", RowMeta::default()),
        (
            "source_only",
            RowMeta {
                src: measured(11, 1_500_000_000_000),
                dst: None,
            },
        ),
        (
            "target_only",
            RowMeta {
                src: None,
                dst: measured(22, 1_550_000_000_000),
            },
        ),
        (
            "both_sides",
            RowMeta {
                src: measured(11, 1_500_000_000_000),
                dst: measured(22, 1_550_000_000_000),
            },
        ),
    ]
}

fn action_token(action: &Action) -> String {
    serde_json::to_string(action)
        .expect("a plan action serializes as its wire token")
        .trim_matches('"')
        .to_owned()
}

fn owned_side_paths(operation: &Op) -> (Option<String>, Option<String>) {
    let (source, target) = side_paths(operation);
    (source.map(str::to_owned), target.map(str::to_owned))
}

fn vectors() -> Vec<RuleVector> {
    let actions = [
        Action::Copy,
        Action::Update,
        Action::Move,
        Action::Delete,
        Action::DeleteDir,
        Action::Chmod,
        Action::Conflict,
        Action::Note,
    ];
    let mut vectors = Vec::new();
    for action in &actions {
        for side in [Side::Source, Side::Target] {
            for profile in &PROFILES {
                for (metadata_name, metadata) in metadata_cases() {
                    let operation = Op {
                        side: side.clone(),
                        action: action.clone(),
                        path: profile.path.to_owned(),
                        from: profile.from.map(str::to_owned),
                        size: profile.size,
                        mtime_ms: profile.mtime_ms,
                        hash: profile.hash.map(str::to_owned),
                        link: profile.link.map(str::to_owned),
                        mode: profile.mode,
                        reason: "vector".to_owned(),
                    };
                    let reversed = reverse_op(&operation, &metadata);
                    vectors.push(RuleVector {
                        name: format!(
                            "{}/{}/{}/{metadata_name}",
                            action_token(action),
                            side.as_str(),
                            profile.name,
                        ),
                        action_rank: operation.action.plan_rank(),
                        side_paths: owned_side_paths(&operation),
                        reversed_side_paths: reversed.as_ref().map(owned_side_paths),
                        reconstructed_meta: row_meta(&operation, None),
                        op: operation,
                        meta: metadata,
                        reversed,
                    });
                }
            }
        }
    }
    vectors
}

/// ts-rs resolves every `export_to` against `TS_RS_EXPORT_DIR`, and every wire type in this
/// repository is exported to `../Dev/typescript/core/types/generated/`. Deriving the directory the
/// same way is what lets `Script/gen-types.mjs --check` redirect this emitter into its scratch tree
/// along with the rest; writing to a repository-relative path instead would mutate the working tree
/// during a read-only check and report the file as permanently stale.
fn output_directory() -> PathBuf {
    let export_directory = PathBuf::from(
        std::env::var("TS_RS_EXPORT_DIR").unwrap_or_else(|_| "./bindings".to_owned()),
    );
    export_directory
        .parent()
        .expect("the ts-rs export directory always has a parent")
        .join("Dev/typescript/core/types/generated")
}

const HEADER: &str = concat!(
    "// Generated from Dev/src/workflow/pipeline/compare/tests/rule_vectors.rs by `npm run gen:types`.\n",
    "// Do not edit this file manually.\n",
    "//\n",
    "// One Rust-evaluated case per entry of the compare-plan rules that\n",
    "// Dev/typescript/core/domain/compare/plan.ts re-implements for the desktop table.\n",
    "// Script/tests/compare-plan-rules.test.mts replays every entry against that copy.\n",
    "import type { Op } from \"./Op\";\n",
    "import type { RowMeta } from \"./RowMeta\";\n",
    "\n",
    "export type ComparePlanRuleVector = {\n",
    "  name: string,\n",
    "  op: Op,\n",
    "  meta: RowMeta,\n",
    "  action_rank: number,\n",
    "  reversed: Op | null,\n",
    "  side_paths: [string | null, string | null],\n",
    "  reversed_side_paths: [string | null, string | null] | null,\n",
    "  reconstructed_meta: RowMeta,\n",
    "};\n",
    "\n",
    "export const COMPARE_PLAN_RULE_VECTORS: ComparePlanRuleVector[] = ",
);

#[test]
fn export_bindings_compare_plan_rule_vectors() {
    let cases = vectors();
    let mut body = String::from("[\n");
    for (position, case) in cases.iter().enumerate() {
        body.push_str("  ");
        body.push_str(&serde_json::to_string(case).expect("a rule vector serializes"));
        if position + 1 < cases.len() {
            body.push(',');
        }
        body.push('\n');
    }
    body.push_str("];\n");

    let directory = output_directory();
    std::fs::create_dir_all(&directory).expect("the generated contract directory is writable");
    std::fs::write(
        directory.join("comparePlanRuleVectors.ts"),
        format!("{HEADER}{body}"),
    )
    .expect("the compare-plan rule vectors are writable");
}
