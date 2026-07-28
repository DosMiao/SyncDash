//! The cases themselves — one mutation, one expected plan, one expected tree.
//!
//! Expectations are typed rather than stringly: `Action::Move`, a `Side`, a `Need`, a `Bytes`
//! variant. That is the whole reason this is Rust and not a manifest — adding a mutation without
//! saying what it should do is a compile error, and a reason string that drifts fails a build
//! instead of quietly matching nothing. The alternative would be a second interpreter for a
//! fixture format, which is exactly the mirrored-semantics maintenance debt this codebase has
//! already paid for once elsewhere.
//!
//! Reasons are matched as prefixes, so a case pins as much of the string as it means to.

use super::corpus::{Seed, BASE, BLIND_OFFSET, SAMPLING, SEEN_OFFSET};
use super::{Bytes, Case, Expect, ExpectOp, Need};
use crate::model::plan::{Action, Side};

const T0: i64 = 1_767_225_600_000;

/// Every case, run by every lane.
pub const ALL: &[Case] = &[
    // The false-positive floor. If two identical trees produce work, nothing below this line can
    // be trusted — every other case's op list would be measuring noise.
    Case {
        name: "identical_trees_produce_no_ops",
        seeds: BASE,
        source_edits: &[],
        target_edits: &[],
        mode: "mirror",
        rigor: "standard",
        needs: &[],
        expect: Expect {
            ops: &[],
            bytes: Bytes::None,
            target_equals_source: true,
            extra_on_target: &[],
            preserved: &[],
        },
    },
    Case {
        name: "a_new_file_copies",
        seeds: BASE,
        source_edits: &[super::corpus::Edit::Add(Seed {
            path: "docs/notes-new.md",
            seed: 50,
            size: 2_048,
            mtime_ms: T0 + 60_000,
        })],
        target_edits: &[],
        mode: "mirror",
        rigor: "standard",
        needs: &[],
        expect: Expect {
            ops: &[ExpectOp {
                side: Side::Target,
                action: Action::Copy,
                path: "docs/notes-new.md",
                from: None,
                reason: "only-in-source",
            }],
            bytes: Bytes::AtLeast(2_048),
            target_equals_source: true,
            extra_on_target: &[],
            preserved: &[],
        },
    },
    // Rewrite changes the size as well as the bytes, so every rigor tier can see it — the
    // same-size case is a separate, harder one that belongs with the sampling cases.
    Case {
        name: "changed_content_updates",
        seeds: BASE,
        source_edits: &[super::corpus::Edit::Rewrite(Seed {
            path: "docs/readme.md",
            seed: 51,
            size: 1_500,
            mtime_ms: T0 + 3_600_000,
        })],
        target_edits: &[],
        mode: "mirror",
        rigor: "standard",
        needs: &[],
        expect: Expect {
            ops: &[ExpectOp {
                side: Side::Target,
                action: Action::Update,
                path: "docs/readme.md",
                from: None,
                reason: "differs-master-wins",
            }],
            bytes: Bytes::AtLeast(1_500),
            target_equals_source: true,
            extra_on_target: &[],
            preserved: &["docs/readme.md"],
        },
    },
    // A delete must both remove the file and keep a copy: this backend cannot reach the central
    // trash store, so the original lands under the root's own `.syncdash/trash/<run>/`.
    Case {
        name: "a_removed_file_deletes_and_is_preserved",
        seeds: BASE,
        source_edits: &[super::corpus::Edit::Delete("archive-me/old1.txt")],
        target_edits: &[],
        mode: "mirror",
        rigor: "standard",
        needs: &[],
        expect: Expect {
            ops: &[ExpectOp {
                side: Side::Target,
                action: Action::Delete,
                path: "archive-me/old1.txt",
                from: None,
                reason: "gone-from-source",
            }],
            bytes: Bytes::None,
            target_equals_source: true,
            extra_on_target: &[],
            preserved: &["archive-me/old1.txt"],
        },
    },
    // The feature the tool was built around. All three legs of the proof are here: the plan says
    // Move with the same-parent reason, no bytes crossed, and nothing was preserved — a
    // copy-and-delete would leave the identical tree but fail the second and third.
    Case {
        name: "rename_in_place_is_a_move_not_a_copy",
        seeds: BASE,
        source_edits: &[super::corpus::Edit::Rename {
            from: "code/lib/util.rs",
            to: "code/lib/helpers.rs",
        }],
        target_edits: &[],
        mode: "mirror",
        rigor: "standard",
        needs: &[],
        expect: Expect {
            ops: &[ExpectOp {
                side: Side::Target,
                action: Action::Move,
                path: "code/lib/helpers.rs",
                from: Some("code/lib/util.rs"),
                reason: "rename-detected-by-hash",
            }],
            bytes: Bytes::None,
            target_equals_source: true,
            extra_on_target: &[],
            preserved: &[],
        },
    },
    // Relocating a file to a different parent is a move, not a rename — different reason string,
    // and the emptied directories have to be cleaned up deepest-first.
    Case {
        name: "relocation_across_directories_is_a_move",
        seeds: BASE,
        source_edits: &[super::corpus::Edit::Rename {
            from: "nested/a/b/c/deep.txt",
            to: "nested/x/y/z/deep.txt",
        }],
        target_edits: &[],
        mode: "mirror",
        rigor: "standard",
        needs: &[],
        expect: Expect {
            ops: &[
                ExpectOp {
                    side: Side::Target,
                    action: Action::Move,
                    path: "nested/x/y/z/deep.txt",
                    from: Some("nested/a/b/c/deep.txt"),
                    reason: "move-detected-by-hash",
                },
                ExpectOp {
                    side: Side::Target,
                    action: Action::DeleteDir,
                    path: "nested/a/b/c",
                    from: None,
                    reason: "dir-gone-from-source",
                },
                ExpectOp {
                    side: Side::Target,
                    action: Action::DeleteDir,
                    path: "nested/a/b",
                    from: None,
                    reason: "dir-gone-from-source",
                },
                ExpectOp {
                    side: Side::Target,
                    action: Action::DeleteDir,
                    path: "nested/a",
                    from: None,
                    reason: "dir-gone-from-source",
                },
            ],
            bytes: Bytes::None,
            target_equals_source: true,
            extra_on_target: &[],
            preserved: &[],
        },
    },
    // Every zero-length file shares one blake3, so pairing them by content would marry unrelated
    // `.gitkeep`s. The detector refuses on `size > 0`; this asserts the refusal rather than
    // assuming it, and the original being preserved is what proves it took the copy+delete route.
    Case {
        name: "a_zero_length_file_never_pairs_as_a_move",
        seeds: BASE,
        source_edits: &[super::corpus::Edit::Rename {
            from: "empty/zero-1.dat",
            to: "empty/moved-zero.dat",
        }],
        target_edits: &[],
        mode: "mirror",
        rigor: "standard",
        needs: &[],
        expect: Expect {
            ops: &[
                ExpectOp {
                    side: Side::Target,
                    action: Action::Copy,
                    path: "empty/moved-zero.dat",
                    from: None,
                    reason: "only-in-source",
                },
                ExpectOp {
                    side: Side::Target,
                    action: Action::Delete,
                    path: "empty/zero-1.dat",
                    from: None,
                    reason: "gone-from-source",
                },
            ],
            bytes: Bytes::None,
            target_equals_source: true,
            extra_on_target: &[],
            preserved: &["empty/zero-1.dat"],
        },
    },
    // Three byte-identical candidates for one arrival. Ambiguity is counted over the *deletions*
    // in the hash bucket, so all three twins have to go and one come back — renaming a single one
    // leaves a bucket of one and is not ambiguous at all. The detector still moves one, but it has
    // to say the attribution was a guess rather than presenting it as fact.
    Case {
        name: "an_ambiguous_pairing_says_so",
        seeds: BASE,
        source_edits: &[
            super::corpus::Edit::Delete("dupes/twin-1.bin"),
            super::corpus::Edit::Delete("dupes/twin-2.bin"),
            super::corpus::Edit::Delete("dupes/twin-3.bin"),
            super::corpus::Edit::Add(Seed {
                path: "dupes/relocated.bin",
                seed: 99,
                size: 4_096,
                mtime_ms: T0 + 9_000,
            }),
        ],
        target_edits: &[],
        mode: "mirror",
        rigor: "standard",
        needs: &[],
        expect: Expect {
            ops: &[
                ExpectOp {
                    side: Side::Target,
                    action: Action::Move,
                    path: "dupes/relocated.bin",
                    from: Some("dupes/twin-1.bin"),
                    reason: "rename-detected-by-hash (ambiguous: 3 identical candidates)",
                },
                ExpectOp {
                    side: Side::Target,
                    action: Action::Delete,
                    path: "dupes/twin-2.bin",
                    from: None,
                    reason: "gone-from-source",
                },
                ExpectOp {
                    side: Side::Target,
                    action: Action::Delete,
                    path: "dupes/twin-3.bin",
                    from: None,
                    reason: "gone-from-source",
                },
            ],
            bytes: Bytes::None,
            target_equals_source: true,
            extra_on_target: &[],
            preserved: &["dupes/twin-2.bin", "dupes/twin-3.bin"],
        },
    },
    // `quick` reads no bytes, so the snapshot is unhashed, so the move detector never runs. The
    // content still ends up right — but the rename costs a full re-transfer and the plan says
    // Copy+Delete. That degradation is the point: assert it, so nobody assumes move detection is
    // tier-independent.
    Case {
        name: "quick_rigor_loses_move_detection",
        seeds: BASE,
        source_edits: &[super::corpus::Edit::Rename {
            from: "code/lib/util.rs",
            to: "code/lib/helpers.rs",
        }],
        target_edits: &[],
        mode: "mirror",
        rigor: "quick",
        needs: &[],
        expect: Expect {
            ops: &[
                ExpectOp {
                    side: Side::Target,
                    action: Action::Copy,
                    path: "code/lib/helpers.rs",
                    from: None,
                    reason: "only-in-source",
                },
                ExpectOp {
                    side: Side::Target,
                    action: Action::Delete,
                    path: "code/lib/util.rs",
                    from: None,
                    reason: "gone-from-source",
                },
            ],
            bytes: Bytes::AtLeast(8_192),
            target_equals_source: true,
            extra_on_target: &[],
            preserved: &["code/lib/util.rs"],
        },
    },
    // --- the evidence tiers, at the 4 MiB sampling floor ---
    //
    // These are the cases where the tool is allowed to be wrong, and the point is to pin exactly
    // how wrong. `standard` reads three 256 KiB windows of a large file, so an edit between them is
    // genuinely unseen. That is a documented trade, not a defect — but it is only honest while it
    // is *measured*, and while `paranoid` is known to close it.
    Case {
        name: "a_blind_region_edit_is_invisible_to_sampling",
        seeds: SAMPLING,
        source_edits: &[super::corpus::Edit::Patch {
            path: "big/handbook.bin",
            at: BLIND_OFFSET,
            xor: 0xFF,
        }],
        target_edits: &[],
        mode: "mirror",
        rigor: "standard",
        needs: &[Need::RangedRead, Need::SetMtime],
        expect: Expect {
            ops: &[],
            bytes: Bytes::None,
            // Deliberately false: the two trees really do differ, and the tool really does not
            // notice. Asserting equality here would be asserting a lie.
            target_equals_source: false,
            extra_on_target: &[],
            preserved: &[],
        },
    },
    Case {
        name: "paranoid_sees_what_sampling_cannot",
        seeds: SAMPLING,
        source_edits: &[super::corpus::Edit::Patch {
            path: "big/handbook.bin",
            at: BLIND_OFFSET,
            xor: 0xFF,
        }],
        target_edits: &[],
        mode: "mirror",
        rigor: "paranoid",
        needs: &[Need::SetMtime],
        expect: Expect {
            ops: &[ExpectOp {
                side: Side::Target,
                action: Action::Update,
                path: "big/handbook.bin",
                from: None,
                reason: "differs-master-wins",
            }],
            bytes: Bytes::AtLeast(6 * 1_048_576),
            target_equals_source: true,
            extra_on_target: &[],
            preserved: &["big/handbook.bin"],
        },
    },
    // The control that stops the case above from passing for the wrong reason: the same one-byte
    // edit, moved inside the head window, must be caught at the sampled tier.
    Case {
        name: "an_edit_inside_a_sampling_window_is_caught",
        seeds: SAMPLING,
        source_edits: &[super::corpus::Edit::Patch {
            path: "big/handbook.bin",
            at: SEEN_OFFSET,
            xor: 0xFF,
        }],
        target_edits: &[],
        mode: "mirror",
        rigor: "standard",
        needs: &[Need::RangedRead, Need::SetMtime],
        expect: Expect {
            ops: &[ExpectOp {
                side: Side::Target,
                action: Action::Update,
                path: "big/handbook.bin",
                from: None,
                reason: "differs-master-wins",
            }],
            bytes: Bytes::AtLeast(6 * 1_048_576),
            target_equals_source: true,
            extra_on_target: &[],
            preserved: &["big/handbook.bin"],
        },
    },
    // The floor is exact, not approximate. Both files get the identical edit at the identical
    // offset; the only difference is one byte of length. The 4 MiB file samples and misses it, the
    // file one byte smaller is read whole and catches it.
    Case {
        name: "the_sampling_floor_is_exact_to_the_byte",
        seeds: SAMPLING,
        source_edits: &[
            super::corpus::Edit::Patch { path: "big/at-4mib.bin", at: BLIND_OFFSET, xor: 0xFF },
            super::corpus::Edit::Patch {
                path: "big/at-4mib-minus1.bin",
                at: BLIND_OFFSET,
                xor: 0xFF,
            },
        ],
        target_edits: &[],
        mode: "mirror",
        rigor: "standard",
        needs: &[Need::RangedRead, Need::SetMtime],
        expect: Expect {
            ops: &[ExpectOp {
                side: Side::Target,
                action: Action::Update,
                path: "big/at-4mib-minus1.bin",
                from: None,
                reason: "differs-master-wins",
            }],
            bytes: Bytes::AtLeast(4 * 1_048_576 - 1),
            target_equals_source: false,
            extra_on_target: &[],
            preserved: &["big/at-4mib-minus1.bin"],
        },
    },
    // enrich fills gaps and never removes: the file the source dropped must survive on the target.
    Case {
        name: "enrich_never_deletes",
        seeds: BASE,
        source_edits: &[super::corpus::Edit::Delete("archive-me/old1.txt")],
        target_edits: &[],
        mode: "enrich",
        rigor: "standard",
        needs: &[],
        expect: Expect {
            ops: &[],
            bytes: Bytes::None,
            target_equals_source: false,
            extra_on_target: &[],
            preserved: &[],
        },
    },
];
