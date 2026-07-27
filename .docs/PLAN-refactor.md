# SyncDash 结构优化计划 / Codebase Refactor Plan

> Scope: whole app — `src/` (11.4k lines Rust), `src-tauri/` (886), `typescript/` (2.8k).
> Method: six parallel deep-read agents over every file, plus git churn/co-change analysis.
> Status: proposal. Nothing below has been applied.

---

## 0. Why this is worth doing (the empirical case)

Line counts alone don't justify a refactor. The git history does.

**Churn ranks the top five files in exactly the order of their size**, and the co-change pairs show why:

```
typescript/main.ts    <->  typescript/styles.css   14 co-commits
src/main.rs           <->  src/run.rs              12
src/config.rs         <->  src/main.rs             12
src-tauri/src/main.rs <->  typescript/main.ts      12
src/config.rs         <->  src/run.rs              11
```

Adding one sync option today means editing a **six-file vertical**:
`config.rs` → `run.rs` → `main.rs` → `src-tauri/main.rs` → `main.ts` → `styles.css`.

That is the cost the current structure imposes. Every target below is chosen to shorten that
vertical, not to hit a line-count quota.

### The safety net is inverted

All 115 tests are inline `#[cfg(test)]` blocks; there is no `tests/` directory.

| Layer | Code lines | Tests | Verdict |
|---|---:|---:|---|
| `compare.rs` | 1018 | 37 | well guarded |
| `apply.rs` | 699 | 13 | well guarded |
| `filter.rs` | 400 | 11 | well guarded |
| **`main.rs` (CLI)** | **868** | **0** | unguarded |
| **`run.rs`** | **747** | **0** | unguarded |
| **`pack.rs`** | **471** | **0** | unguarded |
| **`table.rs`** | 157 | **0** | unguarded, highest fan-in leaf |
| **`typescript/main.ts`** | **2304** | **0** | unguarded |

The **biggest files are the least protected**. This dictates the phase order in §4: mechanical
moves that the compiler verifies come first; the untested orchestrators get characterization
tests *before* they are touched.

---

## 1. Target architecture

Five strict layers. Dependencies point **down only**. Two deliberate exceptions are documented
in §1.3 rather than engineered away.

```
L4  shells        src/bin/syncdash (CLI)   ·   src-tauri (desktop)
L3  orchestration config/   run/
L2  domain        filter/  scan/  compare/  apply/  transfer/  guard/  lock  remote
L1  services      obs/  store/
L0  foundation    foundation/  sys/  model/
```

### 1.1 Proposed tree

Sizes are estimates of code+tests per file after the split.

```
src/
├─ lib.rs                        module declarations only

├─ foundation/                   L0 — zero crate deps, pure functions
│   ├─ mod.rs
│   ├─ fmt.rs        ~120        human_bytes, pct, duration        [1 impl, was 3+3]
│   ├─ time.rs       ~140        now_ms, civil_from_days, stamp    [1 impl, was 3]
│   ├─ path.rs       ~180        to_native, to_rel, parent, base, sep, rel_is_safe
│   ├─ text.rs        ~90        norm_key (NFC+fold), fold         [fixes the NFC divergence]
│   ├─ jsonl.rs      ~150        read/write codec                  [was ~30 open-coded sites]
│   └─ names.rs       ~60        .syncdash.tmp. / .lock / -root / .version_syncDash / runs.jsonl

├─ sys/                          L0 — the ONLY #[cfg] in the crate
│   ├─ mod.rs
│   ├─ fs.rs         ~200        set_mode, read_mode, symlink, file_id, mtime, exists_no_follow
│   ├─ disk.rs       ~110        disk_space (statvfs / GetDiskFreeSpaceExW)
│   ├─ sched.rs       ~90        init_worker_pool, lower_thread_priority
│   └─ hash.rs       ~130        blake3 file/stream/bytes           [was 23 open-coded sites]

├─ model/                        L0 — the shared vocabulary
│   ├─ mod.rs
│   ├─ entry.rs      ~170        Entry, EntryKind, Header, Snapshot, SCHEMA
│   ├─ plan.rs       ~230        Action(+is_executable), Op, Side(+Display), Plan, PlanHeader
│   ├─ generation.rs  ~90        ARCHIVE_GENERATIONS, roll_generations, generation_of
│   └─ options.rs    ~180        CompareOptions, ApplyOptions, ScanOptions, Guards, ConflictPolicy

├─ obs/                          L1 — observability
│   ├─ mod.rs
│   ├─ event.rs      ~200        ProgressEvent, Phase, LogLevel, ItemOutcome  ← LEAF, + Deserialize
│   ├─ ctl.rs        ~190        RunCtl, RunCtx, cancelled_err, is_cancelled
│   ├─ sink.rs       ~230        ProgressSink, registry, Multi/Stderr/Tally/Null
│   ├─ file_sink.rs  ~220        FileSink, AppLogSink, Appender
│   └─ macros.rs      ~40        log_info! / log_warn! / log_error!

├─ store/                        L1 — on-disk state
│   ├─ mod.rs
│   ├─ paths.rs      ~110        jobs_dir, config_dir, cache_dir, trash_root, logs_dir
│   ├─ hashcache.rs  ~130        [from scan.rs:81-114,173-187]
│   ├─ mtimefix.rs   ~110        [from scan.rs:116-171 — breaks apply→scan]
│   ├─ trash/                    store.rs (read) · stash.rs (write, from apply) · retention.rs
│   ├─ versions/                 writer.rs · index.rs · restore.rs · prune.rs
│   └─ runlog/                   record.rs · recorder.rs · read.rs · retention.rs · migrate.rs

├─ filter/                       L2
│   ├─ mod.rs        ~190        PathFilter, pass_file, pass_dir, is_deletable
│   ├─ mask.rs       ~220        matches_mask, mask_begin_wild, Masks, parse_phrase
│   ├─ presets.rs     ~80        SELF/OS/DEV excludes → reference foundation::names
│   └─ query.rs       ~60        mask_hits (UI funnel)

├─ scan/                         L2
│   ├─ mod.rs        ~180        ScanOptions, scan(), scan_ctx()
│   ├─ walk.rs       ~240        walkdir loop, exclusion counting, long-path prefix
│   ├─ hash_phase.rs ~200        rayon hashing phase
│   └─ sample.rs     ~110        sampled_digest, SAMPLE_MIN, effective_read

├─ compare/                      L2  ← from one 1575-line file
│   ├─ mod.rs        ~180        compare() orchestrator + CompareCtx
│   ├─ keys.rs       ~130        norm_key, files_equal, map_of
│   ├─ moves.rs      ~190        MovePair, detect_moves, move_reason
│   ├─ mirror.rs     ~140        mirror + enrich decisions
│   ├─ sync.rs       ~290        sync decisions + generation conflict classifier
│   ├─ escalate.rs   ~180        ← from run.rs:134-210 (the 6th pass, currently exiled)
│   ├─ order.rs       ~70        rank/sort  [shared with apply — was duplicated]
│   ├─ evidence.rs   ~220        evidence(), same_page(), reverse_op   (GUI-facing)
│   └─ pass/
│       ├─ attrs.rs  ~180        symlink + unix-mode passes
│       ├─ conflict.rs ~250      naming + resolution + max_conflicts
│       └─ hygiene.rs ~140       case-collision + Windows-path legality

├─ apply/                        L2
│   ├─ mod.rs        ~230        apply_with orchestration
│   ├─ exec/
│   │   ├─ copy.rs   ~220        stage → transfer → verify → stamp → commit
│   │   ├─ mutate.rs ~130        move / chmod / delete
│   │   └─ dir.rs    ~140        DirOutcome, try_delete_dir
│   ├─ preserve.rs   ~110        trash-vs-versioning policy  [removes apply→version]
│   ├─ schedule.rs   ~140        run_class, phase classification
│   ├─ ledger.rs     ~160        record(), outcome classification
│   ├─ preview.rs     ~90        dry-run path
│   └─ staged.rs     ~290        ← atomic.rs, renamed (see §1.4)

├─ transfer/                     L2
│   ├─ mod.rs
│   ├─ chunk.rs      ~110        FastCDC, ChunkInfo, FileChunks
│   ├─ recipe.rs     ~200        RecipeStep + build + apply   ← breaks version→pack, unifies 3 impls
│   ├─ delta.rs      ~130        update_with_delta
│   └─ pack/
│       ├─ format.rs ~180        Manifest, PayloadEntry, tar_header, PACK_VERSION
│       ├─ write.rs  ~200        pack()
│       └─ read.rs   ~240        extract + verify (orchestration moves to run/)

├─ guard/                        L2  (was preflight)
│   ├─ mod.rs        ~130        run_all, Verdict (no side-effecting .report())
│   ├─ marker.rs     ~110        MARKER_NAME, read/write
│   ├─ space.rs       ~70        check_space
│   ├─ ratio.rs       ~90        check_delete_ratio
│   ├─ roots.rs      ~150        check_root + same/nested-root  ← from src-tauri:298-345
│   └─ stats.rs      ~120        stat_plan, PlanStats, SideStats

├─ lock.rs           ~100        L2 — unchanged, but export LOCK_NAME
├─ remote.rs         ~140        L2 — unchanged + ProbeInfo type

├─ config/                       L3
│   ├─ mod.rs
│   ├─ job.rs        ~260        Job + Default + SAMPLE   (pure data → becomes a leaf)
│   ├─ rigor.rs      ~170        RigorResolved::from_parts  ← unifies main.rs:596 + config.rs:181
│   ├─ options.rs    ~120        guards()/compare_opts()/apply_opts() builders
│   └─ settings.rs   ~230        AppSettings (migrate_log_dir moves to store/runlog/)

└─ run/                          L3
    ├─ mod.rs
    ├─ dispatch.rs    ~80        local-vs-remote  ← collapses 6 replicated branches
    ├─ compare.rs    ~230        compare_job_detailed + remote variant
    ├─ apply.rs      ~240        apply_job_guarded_with + remote variant
    ├─ session.rs    ~150        RunSession: begin_run / Recorder / end_run  ← shared by both shells
    ├─ archive.rs    ~110        refresh_archive_with
    └─ pack.rs       ~220        apply_pack orchestration  ← from pack.rs:236-471

src/bin/syncdash/               L4 — CLI (was src/main.rs, 868 lines)
├─ main.rs           ~90        bootstrap, console detach, dispatch, exit codes
├─ args.rs          ~320        the entire clap schema (pure declaration)
├─ exit.rs           ~60        one exit-code table  [was 23 scattered sites]
├─ launch.rs         ~60        desktop-binary discovery + spawn
├─ render/                      logs.rs · history.rs · trash.rs · plan.rs · progress_bar.rs
└─ cmd/                         one module per subcommand group
    └─ territory.rs             ← moved out of the library (CLI-only, 1 inbound ref)
```

### 1.2 Frontend

`typescript/main.ts` is 2304 lines with **no state store** — ~26 module-level `let`s and six
parallel arrays (`ops` / `reversed` / `metas` / `checked` / `flipped` / `maskHit`) that must be
kept length-synced by hand. Three separate sites reset overlapping subsets of them.

```
typescript/
├─ main.ts            ~120      bootstrap + wiring only
├─ shared/                      imported by BOTH windows  [was copy-paste]
│   ├─ dom.ts                   $, escapeHtml            [byte-identical dupes today]
│   ├─ format.ts                bytes, duration, time    [5 formatters in main.ts alone]
│   ├─ prefs.ts                 typed localStorage `sd.*`
│   └─ ipc/
│       ├─ generated.ts         ← GENERATED from Rust (§3.2). Do not hand-edit.
│       └─ client.ts            typed invoke/listen wrappers
├─ state/
│   ├─ store.ts                 one plan-row model, replaces the 6 parallel arrays
│   └─ selectors.ts             visibleIdx, funnel, sort  [memoized — 8 O(n) call sites today]
├─ panels/
│   ├─ job-list.ts     ├─ diff-table.ts   ├─ overview.ts
│   ├─ job-editor/     ├─ settings.ts     ├─ log-console.ts
│   ├─ funnel.ts       ├─ same-items.ts   └─ compare-panel.ts
├─ actions/
│   ├─ compare.ts      ├─ sync.ts         ├─ watch.ts
│   └─ job-mutations.ts           ← swap / addExclude / fp-promote are 3 copies of one
│                                    read-modify-write-undo transaction (~90 lines → ~25)
├─ progress.ts        ~300      keeps its own logic, drops its duplicated helpers
└─ styles/                      tokens.css + one file per panel (see §1.5)
```

### 1.3 Accepted reverse edges

Per the layering rule, reverse references are acceptable when necessary. Two are kept:

1. **`obs/ctl.rs` → `obs/sink.rs` registry.** `RunCtx::null()` must resolve the process-global
   sink or the CLI silently loses its mount-point warnings. Kept *inside* `obs/`, so it is an
   intra-module edge rather than a cross-layer cycle.
2. **`transfer/pack` ← `run/pack.rs`.** `apply_pack` is orchestration, so it moves *up* to `run/`.
   The remaining `pack → apply` edge disappears rather than being inverted.

All three current cycles are **eliminated**, each by moving one item (§2.1).

### 1.4 Naming decisions

- **`atomic.rs` → `apply/staged.rs`.** `crate::atomic` reads as `std::sync::atomic`, which
  `apply.rs` and `scan.rs` both genuinely import. The module is about staged writes, not atomics.
- **`preflight/` → `guard/`.** Its fan-in of 28 is inflated: most importers want only
  `human_bytes`. Once that moves to `foundation::fmt`, real fan-in drops to ~6.
- **No barrel files.** Every `mod.rs` declares submodules and holds real orchestration code.
  Callers import concrete paths (`compare::moves::detect_moves`), not flattened re-exports.
  Same rule on the TS side: no `index.ts` re-export hubs.

### 1.5 File-size rule

Target **150–350 lines** including inline tests. Deviations must be justified:
- `compare/sync.rs` (~290) — the generation conflict classifier is one decision matrix; splitting
  it would separate arms of a single `match`.
- `foundation/names.rs` (~60) — deliberately tiny. It exists to be depended on by everything
  without dragging anything in; padding it would defeat that.

---

## 2. What moves, and why

### 2.1 The three dependency cycles — each is a one-item fix

| Cycle | Fix | Cost |
|---|---|---|
| `progress ⇄ logging` | Move the sink registry into `obs/`; `event.rs` becomes a true leaf | 1 line each way |
| `settings ⇄ runlog` | Hoist `INDEX_FILE`/`SUMMARY_FILE`/`PLAN_FILE` to `foundation::names` | 1 constant |
| `apply → version → pack → apply` | Move `RecipeStep` from `pack` to `transfer/recipe.rs` | 1 type |

*Verified:* `progress.rs:184` ↔ `logging.rs:15`; `runlog.rs:72,158,289` ↔ `settings.rs:169,305`;
`version.rs:16` → `pack.rs:19` → `pack.rs:447` → `apply`.

After these three, the Rust graph is a clean DAG. **Do this first** — it is cheap, compiler-verified,
and every later phase gets easier.

### 2.2 Code that should move OUT

| Item | From | To | Why |
|---|---|---|---|
| `human_bytes` | `preflight.rs:107` | `foundation/fmt.rs` | A byte formatter in a safety-gate module. It is the *sole* reason `apply.rs` and `trash.rs` import `preflight` at all. |
| `escalate_sampled_disagreements` | `run.rs:134-210` | `compare/escalate.rs` | 77 lines of comparison semantics living in the orchestrator. It re-derives "equal" and the mode matrix, and indexes on **raw paths** while `compare` indexes on `norm_key` — so it matches NFD/case variants differently. |
| hash cache + mtime-fix store | `scan.rs:81-171` | `store/` | Written by `apply.rs:665,668`, read by `scan` — the reverse `apply → scan` edge exists only because the store lives inside `scan`. |
| `move_to_trash`, `default_trash` | `apply.rs:56-77` | `store/trash/stash.rs` | The trash store's writer and reader are currently in different modules. |
| `apply_pack` orchestration | `pack.rs:236-471` | `run/pack.rs` | 236 lines of orchestration misfiled in a wire-format module; it is what makes `pack` call `apply`. |
| `migrate_log_dir` | `settings.rs:147-253` | `store/runlog/migrate.rs` | It parses `runs.jsonl` inline — `runlog`'s schema knowledge living in `settings`. |
| `init_worker_pool`, `lower_thread_priority` | `scan.rs:35-69` | `sys/sched.rs` | Never called by `scan()`; called by both binaries' `main`. |
| `RecipeStep` | `pack.rs:19` | `transfer/recipe.rs` | See §2.1. |
| `territory.rs` | library | `bin/syncdash/cmd/` | One inbound reference, CLI-only. |
| rigor preset logic | `main.rs:596-616` | `config/rigor.rs` | A **second implementation** of `Job::rigor_resolved()` that omits `escalate` and `verify_writes`. |

### 2.3 Code that should move IN

| Item | From | To | Why |
|---|---|---|---|
| same-root / nested-root detection | `src-tauri:298-345` | `guard/roots.rs` | A real safety rule ("nested roots self-replicate") that **only the GUI has**. `syncdash run` on a nested-root job produces the self-consuming plan with no warning. |
| `civil_from_days` + `stamp` | `src-tauri:393-460` | `foundation/time.rs` | Third copy of the same algorithm, textually divergent (`div_euclid` vs manual branch). The shell carries its own unit test for it. |
| `export_csv` | `src-tauri:377-446` | library, next to `Plan::write_to` | 64 lines of net-new output-format logic (RFC4180, BOM, separator inference) in the shell. Its 55-line test is stranded in the shell binary. |
| `RunSession` (begin/Recorder/end) | duplicated in both shells | `run/session.rs` | `run.rs:316-319,371-374` and `src-tauri:749-764`. |
| local-vs-remote dispatch | 6 sites across both shells | `run/dispatch.rs` | `job.remote_host.is_some()` is the single most replicated line of policy in the repo. |

### 2.4 Duplication inventory

| Concern | Copies | Locations |
|---|---:|---|
| `rel → native` path | **8** | `apply:60`, `chunk:39`, `pack:75`, `run:153`, `territory:62`, `trash:138`, `version:61`, `src-tauri:412` |
| blake3 hash triple | **23** | open-coded across 7 modules; 2 different buffer sizes (8 MiB vs 1 MiB) |
| `civil_from_days` | **3** | `compare:186`, `runlog:92`, `src-tauri:449` — two have separate tests, one has none |
| `SystemTime → ms` | **6** | `apply:85`, `scan:71`, `version:64`, `pack:150`, `atomic:163`, `runlog:534` |
| byte formatting | **3** | `preflight:107` (**KiB/MiB**), `main.ts:164` (**KB/MB**), `progress.ts:156` (identical copy) |
| duration/time formatting | **9** | 4 Rust + 5 in `main.ts` alone |
| rate/ETA smoothing | **4** | `scan:498` (cumulative), `progress.ts:170` (sliding), `main.ts:1704` (EMA), + a 4th in `drawGraph` |
| `Conflict\|Note` op filter | **7** | `run` ×4, `apply:536`, `src-tauri` ×2 — `Action` has no `is_executable()` |
| chunk-recipe build | 2 | `pack:171` ≡ `version:117` |
| chunk-recipe apply | **3** | `pack:364` (streaming), `version:293` (in-memory), `apply:210` (offset-equality) |
| JSONL read/write | ~30 | no shared codec |
| `parent()` / basename | ~11 | hand-rolled `rfind('/')` per site |

**Three of these are behavioral, not cosmetic** — see §5.

### 2.5 Dead code

| Item | Size | Evidence |
|---|---:|---|
| **`vclock.rs`** | 359 lines + 11 tests | Reachable only via `pub mod vclock;` in `lib.rs:23`. Its doc claims it activates under `archive_format = "index"` "see config.rs" — **that key exists nowhere in the repo.** |
| `atomic::sweep_stale_temps` | 24 | zero callers (`TEMP_LIFETIME_MS` exists only for it) |
| `table::matches_any_generation` | 9 | zero callers |
| `version::has_content` | 3 | zero callers |
| 4 Tauri commands | ~40 | `run_history`, `run_detail`, `app_log_tail`, `close_progress_window` — registered, no TS caller |
| `CONFLICT_HINT` (TS) | 3 | declared, never read |
| `jobEl = $('pjob')` (TS) | 1 | declared, never read → **the progress window never shows which job is running** |
| `.ctxitem.danger` (CSS) | 1 | styled + typed + wired, but no menu item ever sets it |

`cargo check` reports zero warnings, but that is **not evidence**: `lib.rs` declares all 23 modules
`pub`, so `dead_code` never fires. Clippy has evidently never been run — there is no config and no CI.

`vclock` is a judgment call: it is a careful, well-tested implementation staged for the v1.0 N-way
work that the README explicitly defers. **Recommendation: keep it, but move it to
`model/vclock.rs` and mark it `#[cfg(feature = "vclock")]`** so it stops reading as live code
while remaining compiled and tested in CI. Fix the false doc claim either way.

---

## 3. The two structural problems worth solving properly

### 3.1 `Op` is constructed by full struct literal 37 times

`compare.rs` documents this as the explicit reason the evidence layer was built as a *parallel
array* rather than adding fields to `Op`. It is the single largest source of both line count and
change-amplification in the file: adding one field means touching 37 sites.

**Fix:** constructor helpers per op shape (`Op::copy`, `Op::del`, `Op::conflict`, `Op::note`, …).
Two already exist locally (`push_copy`, `link_op`) — generalize them. This is a prerequisite for
splitting `compare/`, not a follow-up.

### 3.2 The Rust↔TS contract is hand-maintained in 26 places, and has already drifted

**17 Rust types have hand-written TypeScript twins across 2 files. There is no codegen, no schema,
and no test asserting the two agree.** `Job` alone has four representations that must be edited in
lockstep to add one field: the Rust struct, `JobFull`, `ED_FIELDS`, and `defaultJob()`.

Three disagreements **exist right now**:

| Drift | Consequence |
|---|---|
| `OpDto` omits `link` (`compare.rs:63`) | Works only because TS types are erased and the object round-trips intact. The moment anyone reshapes an op in TS (a spread with explicit keys, a `.map` to a new literal), **symlink ops silently apply as content copies**. No compile error, no test. |
| TS `PlanHeader` is a 9-field subset of a 14-field struct | The missing five have **no `#[serde(default)]`**, so they are required on deserialize. Any TS-side header reconstruction fails at runtime with a serde error and no compile-time signal. |
| `ProgressEvent` mirrored 3× (`RunEv`, `CmpEv`, `LegacyProgress`) | `CmpEv` drops `run_id`, so the compare panel cannot discard late events from a cancelled run. `parseLogLine` ignores `totals`/`paused`/`resumed` and renders them as raw JSON. |

Plus two already-dead contract branches: `action === 'warning'` (retired when `Log{Warn}` replaced
it) and a `legacy_phase` shim that maps the same `Phase` enum to a **different** vocabulary
(`comparing` vs `compare`) on a second channel to the same webview.

`npm run typecheck` passes cleanly — and green-lights all of it, because every `invoke<T>()` is an
unchecked assertion and `$()` is `as T` with no null check across 112 ids.

**Fix: generate `shared/ipc/generated.ts` from the Rust structs** (`ts-rs` or `specta`), and replace
bare `invoke<T>` with generated per-command wrappers. This converts 27 command signatures and 17
types from "trust the author" to "fails the build," and deletes ~350 lines of hand-written TS.

**This is the highest-leverage single change in the plan.** It directly shortens the six-file
vertical from §0: `src-tauri/main.rs ↔ typescript/main.ts` is the #4 co-change pair precisely
because the contract is duplicated by hand.

---

## 4. Phased execution

Each phase ends green: `cargo check --workspace`, `cargo test --workspace` (115 tests),
`npm run typecheck`, `npm run build`.

### Phase 0 — Preconditions (do not skip)

1. **Land or stash the 885 lines of uncommitted work** (556 in `runlog.rs`, plus untracked
   `logging.rs` and `settings.rs`). A large file-shuffling refactor on top of an unlanded feature
   is how a merge becomes unreviewable.
2. **Resync `dist/`** — the tracked `main-DXwpGYWh.js` is deleted on disk and the current build
   `main-D_OND1xW.js` is untracked. The committed frontend does not match the source.
3. Add `.gitattributes` (`* text=auto`, `*.command text eol=lf`) — every Rust file currently warns
   about CRLF conversion.
4. Add `rustfmt.toml`, `clippy.toml`, and a CI workflow running `fmt --check`, `clippy -D warnings`,
   `test --workspace`, `tsc --noEmit`. **There is no automated guard of any kind today.** Land this
   before the refactor so every later phase is checked.
5. Fix the `Cargo.toml` defect (§5.1).

### Phase 1 — Foundation, mechanical (compiler-verified)

Create `foundation/`, `sys/`, `model/`. Move the pure helpers; collapse the duplicate
implementations from §2.4. Break the three cycles from §2.1.

Low risk: every move is a rename the compiler checks. No behavior change except the deliberate
unifications, each of which needs its own test:
- byte formatting picks **one** unit convention (recommend KiB/MiB — the Rust spelling is correct;
  the GUI is the one that's wrong)
- `norm_key` becomes the single case-fold, fixing the NFC divergence (§5.3)

### Phase 2 — Characterization tests for the unguarded orchestrators

Before touching `run.rs`, `pack.rs`, `main.rs`, or `main.ts`, add tests that pin current behavior:
- `run/`: local compare pipeline, remote argv construction, dispatch, archive refresh
- `transfer/pack`: round-trip pack → apply-pack, delta reconstruction, hash gates
- `model/entry.rs` + `model/plan.rs`: JSONL round-trip (**no such test exists today** — and
  `ProgressEvent`/`Phase` derive `Serialize` only, so the persisted log format has *no* Rust parser
  and cannot be round-trip tested at all; add `Deserialize`)
- CLI: golden-output tests for the render layer

This phase adds tests; it moves nothing.

### Phase 3 — Split the domain modules

`compare/`, `apply/`, `scan/`, `filter/`, `guard/`, `transfer/`. These are the well-tested modules,
and their tests are black-box over public APIs — they survive the split verbatim. Two exceptions:
- `scan::sampled_digest` is private and has a test; sampling code and test must move together
- `apply` tests are all end-to-end, so they will not localize a regression — add unit tests for
  `try_delete_dir`, `update_with_delta`, and the fs primitives as they are extracted

Order within the phase: `apply/staged.rs` first (zero crate deps), then `filter`, `scan`, `compare`,
`apply`, `transfer`.

### Phase 4 — Orchestration and the CLI shell

Split `config/` and `run/`. Move the CLI one-shot drivers out of `run.rs` (they are the only part
the desktop never calls, and they hold its only four `println!`). Extract `bin/syncdash/` with
`args.rs`, `render/`, and one `exit.rs` table.

Now protected by Phase 2's tests.

### Phase 5 — IPC codegen

Introduce `ts-rs`/`specta`, generate `shared/ipc/generated.ts`, delete the hand-written mirrors,
fix the three drifts. Retire the `legacy_phase` shim and its dead `'warning'` branch.

### Phase 6 — Frontend split

`shared/` first (immediately deduplicates both windows), then `state/store.ts` to replace the six
parallel arrays, then panels. Resolve the three hard blockers first:
- `renderTable` → `sameOpen()` back-reference across 1550 lines
- `renderJobs`'s click handler calling three functions defined later (works only via hoisting;
  ESM imports turn this into a real cycle)
- `setBusy` writing `btn-swap`, which another section owns

**Hard constraint:** Tauri injects a CSP nonce, which makes browsers ignore `'unsafe-inline'`.
Every dynamic style must go through CSSOM. **Any refactor that reintroduces `style="…"` attributes
white-screens the app.**

### Phase 7 — CSS and docs

Split `styles.css` into `tokens.css` + per-panel files. Extend the token set — there are 12 custom
properties, all colour; no tokens for spacing, radius, font-size, or z-index (bare integers
scattered: 2, 50, 95, 100, 115, 120). Fix the five self-overriding blocks and the two undefined
tokens (`--fg` at :314 has no fallback and silently fails; `--mono` at :317 is saved by its fallback).

Note `.stagerow`, `.btn`, `.chip`, `.ed-field`/`.ed-group` are shared by both windows by explicit
design — they form the shared layer and must not be split into a panel file.

Docs: README is 38 KB — a manual, not a README. Split into an overview plus `docs/`. Move
`PLAN-syncthing-upgrade.md` there too. Record the upstream commits for `.docs/` (the gitignored
28 MB FreeFileSync + syncthing reference corpus), since Chinese comments cite it by file:line
throughout and those citations should stay verifiable.

---

## 5. Defects found during analysis

These are independent of the refactor and several are worth fixing now.

### 5.1 `Cargo.toml`: `libc` is an unconditional dependency — **verified**

The file has a UTF-8 BOM and a mojibake-corrupted comment. The corruption swallowed a newline, so
the table header ended up *inside* the comment:

```toml
# statvfs...Windows...GetDiskFreeSpaceExW...crate?[target.'cfg(unix)'.dependencies]
libc = "0.2"
```

`cargo metadata --no-deps` confirms: **`libc` has `"target": null`.** Windows builds pull in `libc`
for nothing and the `cfg(unix)` gate does not exist. One-line fix. `Cargo.toml` is the only file in
the repo with a BOM or corrupted bytes.

### 5.2 The settings dialog is probably invisible — **in the uncommitted work**

`#setmodal` does not exist in HEAD; the in-flight diff adds `<div id="setmodal" class="hidden">` and
the inner `.setsheet` sizing rule, but **no `#setmodal` rule**. `#modal` (:227) and `#editmodal`
(:333) each get the full `position:fixed; inset:0; background:rgba(0,0,0,.55); display:flex` overlay.
`.sheet` supplies only width/background/border/padding — no positioning.

So with `.hidden` removed, `#setmodal` is a static block after a `height:100%` `#app` under
`body{overflow:hidden}`: below the fold and unscrollable. `main.ts` also does
`if (e.target === setModal)` for click-outside-to-close, which only makes sense for a full-viewport
overlay — confirming the intent. *Reasoned from the cascade; worth a 30-second check in the app.*

### 5.3 NFC divergence lets NFD paths escape excludes

`compare` folds with **NFC + uppercase** (`compare.rs:205`); `filter` folds with **bare uppercase**,
no NFC (`filter.rs:167,329,343,366,393`). `unicode_normalization` is imported *only* in `compare.rs`.

An NFD-spelled path — the macOS/APFS default — can therefore match in `compare` but miss an exclude
mask. Two matchers, two definitions of "same name." Unifying on `foundation::text::norm_key` fixes
it; the change needs a targeted test with a real NFD path.

### 5.4 Three chunk-delta implementations disagree

`apply.rs:210-226` requires **offset equality** to reuse a chunk; `pack.rs:179-181` does not.
`version.rs:293` is a third, in-memory variant. So `apply --delta` and remote-pack delta have
**different hit rates on the same input**. Unifying them in `transfer/recipe.rs` is a behavior
change and needs a deliberate decision on which rule is correct (`pack`'s looser rule appears right).

### 5.5 `panic!` on a user typo

`compare.rs:721`: `other => panic!("unknown mode: {other}")`. `mode` comes straight from user TOML.
A typo panics the whole GUI or CLI instead of returning an error. `Mode` is also typed three
different ways across the codebase (clap `ValueEnum`, `&str`, unvalidated `String`) — and the enum
is converted straight back to `&str` at `main.rs:645`.

Related: `--rigor`, `--evidence`, and `--cache` accept **any** string and silently fall through to a
default. `--rigor paranoyd` degrades to standard with no message; `gen-jobs` writes the unvalidated
string verbatim into the generated TOML.

### 5.6 `apply` and `apply-pack` have no preflight gates

`preflight::run_all` is invoked only from `run.rs:258,220`. So:

| Command | Gated? |
|---|---|
| `syncdash run <job> --apply` | ✅ marker, disk space, delete ratio |
| `syncdash apply plan.jsonl --apply` | ❌ only an ad-hoc `is_dir` check |
| `syncdash apply-pack pkg.tar --apply` | ❌ none |

This is an accident of where `run_all` happens to be called. Whatever the intent, it should be
deliberate — and the refactor will move that call site.

### 5.7 The desktop evaluates preflight three times per Synchronize

`src-tauri:709-713` (the `preflight` command), then `src-tauri:738-745` (inside `apply_job`), then
again inside `run::apply_job_guarded_with` at `run.rs:258`. Each does `stat_plan` plus a
`statvfs`/`GetDiskFreeSpaceExW` syscall. Not a correctness bug — but the second and third failure
paths return *different shapes* (`Err(String)` vs `errors:1`) for the same condition.

### 5.8 `apply_job` ships the plan twice

`main.ts:840` sends `{ name, plan, ops: finalOps, acknowledged }` — `plan` already carries
`ops` + `reversed` + `metas` (3 arrays × n), and `ops` repeats the selected subset. For a 20k-op
plan that is ~80k objects serialized per Synchronize. A `Vec<usize>` of selected indices plus a
server-side plan cache (the `SnapCache` pattern already exists) removes it.

### 5.9 Smaller items

- **`"cancelled"` is a magic error string.** `user_err` returns the literal; `main.ts:759` compares
  `String(e) === 'cancelled'`. Rewording it breaks cancel detection silently.
- **`InvalidData` does 19 different jobs** — bad JSON, hash mismatch, version mismatch, unsafe path,
  bad TOML. Callers get only a string. Since there is exactly one error type today (`io::Result`
  everywhere, no `anyhow`/`thiserror`), introducing `thiserror` is mechanical.
- **`ErrorKind` carries domain meaning**: `Interrupted` = user cancel, `DirectoryNotEmpty` = kept.
  Any error-type refactor **must preserve `ErrorKind` identity** or cancel becomes a hard error.
  Note the two cancel sites correctly conjoin `&& ctl.cancelled()`; the `DirectoryNotEmpty` site
  does not, and is safe only because exactly one place produces it.
- **`version::restore` prints to stdout** from inside the library (`:265,327,337`) — its only
  per-file output channel. Same for ~20 other `println!` sites in `apply`/`pack`/`trash`. In a
  `windows_subsystem="windows"` desktop build these go to a detached console, i.e. nowhere.
  `logging.rs` already built the right escape hatch; these are the un-migrated tail.
- **`package.json` says `0.3.0`** while both `Cargo.toml`s and `tauri.conf.json` say `0.9.0`.
- **`WebviewUrl::App("progress.html")`** (`src-tauri:580`) is a hardcoded string that must equal
  Vite's emitted filename, which Vite derives from the input path *relative to root*. Moving
  `progress.html` into a subdirectory silently breaks the child window with no compile error on
  either side.
- **`dist/` content-hashed filenames generate history bloat** — every frontend rebuild orphans the
  tracked file and adds a new one. The "Mac has no node" rationale is legitimate; pinning
  `rollupOptions.output.entryFileNames` to stable names keeps the benefit without the churn.

---

## 6. Sequencing summary

| Phase | Content | Risk | Guarded by |
|---|---|---|---|
| 0 | Land WIP, resync dist, CI, `.gitattributes`, Cargo.toml fix | — | — |
| 1 | `foundation/` `sys/` `model/`, break 3 cycles, collapse dupes | low | compiler + CI |
| 2 | Characterization tests for `run`/`pack`/`model`/CLI | none | — |
| 3 | Split `compare/` `apply/` `scan/` `filter/` `guard/` `transfer/` | medium | 115 existing tests |
| 4 | Split `config/` `run/`, extract `bin/syncdash/` | medium | Phase 2 tests |
| 5 | IPC codegen, delete hand-written mirrors | medium | generated types + build |
| 6 | Frontend `shared/` → `state/` → `panels/` | high | typecheck + manual |
| 7 | CSS tokens, docs split | low | visual |

Phases 1–2 are independently valuable and can land without committing to the rest. Phase 5 is the
highest-leverage single change and does not depend on Phases 3–4.
