# SyncDash Restructure Plan

> Method: six parallel deep-read agents over every source file, every claim below re-verified
> by hand against the working tree before it was written down.
> Baseline at time of writing: `cargo check --workspace --all-targets` clean, **231 tests green**
> (219 + 12, 1 ignored, 0.72s), `tsc --noEmit` clean, ts-rs output byte-identical on regeneration.
>
> Supersedes the L2-and-above half of `PLAN-refactor.md`. That document's L0/L1 half
> (`foundation/`, `model/`, `obs/`, `store/`, `fs/`) **has been executed**; its frontend phases
> describe a vanilla `main.ts` that no longer exists. Its L2/L3/L4 half was never done — that is
> most of what follows.

---

## 0. What the analysis actually found

The codebase is not disorganized. It has an explicit, written layering doctrine (`src/lib.rs:1-51`),
`foundation/` genuinely holds its zero-in-crate-dependency contract, and there is exactly **one**
re-export block in the whole crate. The problems are narrower and more specific than "it needs a
refactor", and they fall into four groups.

### 0.1 The dominant pattern: consolidations designed, documented as done, never wired

This is the single most repeated finding, and it is why the codebase reads better than it behaves.
Helpers were written in `foundation/` to kill duplicated call sites; the doc comments describe the
kill as complete; the call sites were never repointed.

| Helper | Production callers | Copies still live |
|---|---:|---|
| `foundation::fmt::pct` | **0** | `main.rs:716` (`else {100}`), `src-tauri:205-209` (`else {-1}`), `runstate.ts:100`, `ComparePanel.tsx:36` |
| `foundation::time::stamp_iso` | **0** | `src-tauri:514-524` |
| `foundation::fmt::human_duration` | **0** | `main.rs:427`, `main.rs:1009`, `format.ts:38` |
| `foundation::path::sep_of` | **0** | `src-tauri:530`, `format.ts:56` |
| `foundation::time::civil_from_days` | 7 | `runlog.rs:109`, `src-tauri:570` (+ 2 duplicated tests) |

`foundation/fmt.rs:20-22` names the two divergent percentage fallbacks by shell and says they were
"standardized on 0". **Both are still live and still divergent.**

> **Corrected during Phase 1 — see §7.** Two rows of this table were wrong about the remedy. `pct`
> and `human_duration` do not fit their supposed call sites (`-1` is a live "no percentage" sentinel;
> `human_duration` renders elapsed time, not relative age), so both were **deleted** rather than
> adopted. Adopting them would have been a behavior change disguised as a cleanup.

**One of these is a user-visible bug.** `foundation/fmt.rs:3-4` states the frontend's `KB/MB` +
1024-divisor mismatch was fixed and Rust "is the source of truth". `core/format.ts:9-10` still
divides by 1024 and labels it `MB`. The same byte count renders **`1.5 MiB` in the CLI and
`1.5 MB` in the desktop UI**, across 13 TS call sites.

### 0.2 Dead and mis-shipped code

| Item | Lines | Evidence |
|---|---:|---|
| `model/vclock.rs` | **362** | zero references anywhere; only 3 doc comments + the `pub mod` line |
| `fs/vfs/fake.rs` | **718 prod** | `pub mod fake;` is **ungated** while `conformance` beside it is `#[cfg(test)]`; `fake://` is reachable from any job phrase via `spec::parse` → `vfs::open` |
| `run::materialize_roots` + `materialize_one` | 31 | only 3 test call sites |
| `model::table::matches_any_generation` | ~9 | zero callers |
| `vfs/mod.rs:41` re-export | 1 | zero users — every site writes `vfs::spec::…` |
| `as_vfs_error`, `VfsError::with_source`, `sweep_stale_temps`, `NoPrompt`, `RootLock::acquire`, `scan::record_mtime_fixes`, 4 × `names::*_FILE` | ~55 | zero external callers |

`vclock.rs` holds **all 125 of `model/`'s test lines** and 11 tests, while the 518 production lines
that *are* the on-disk and IPC contract (`table`, `event`, `plan`, `chunk`) have **zero**.

### 0.3 The layering claim is true only at the granularity it was checked

`lib.rs:17-18` claims Tarjan finds "no strongly-connected component larger than one, and no edge
points up the ladder." That verification traverses `use crate::`. The `log_*!` macros are
`#[macro_export]` and expand to `$crate::obs::logging::emit`, so their edges are invisible to it.

Counting them:

```
fs ──log_warn!──▶ obs ──runlog──▶ store ──trash/version──▶ fs
```

- `fs/lock.rs:90`, `fs/vfs/local.rs:232` → `obs::logging`  (**L0 → L1, upward**)
- `obs/runlog.rs:81,165,296` → `store::settings`
- `store/trash.rs:152`, `store/version.rs:146,153,157` → `crate::fs::`

That is an **SCC of size 3** at directory granularity, and two upward edges that `lib.rs:6-7`
requires the module headers to justify — neither header mentions `obs`. This does not need to be
engineered away (the user's rule 5 explicitly allows reverse edges), but the doctrine should say
which granularity it asserts, and the two L0 headers should say why they log.

### 0.4 Presentation is fused into the engine, and it forced a whole shell to be re-implemented

**35 `println!`/`eprintln!` calls sit below the shell layer** (pipeline 11, obs 10, store 8,
transfer 5, model 1) plus 4 in `run.rs`. `run.rs` prints the plan JSONL and the dry-run line
directly, which is precisely why the Tauri shell could not call `run_local_single` and had to
re-implement compare and apply (`src-tauri:763-822`, `848-909`).

The visible cost: the transport branch `if job.remote_host.is_some()` is written out **six times**
across the two shells, the run-log bracket three times, and the pre-apply op filter four times.

### 0.5 File shape

The Rust side is barbell-shaped: **16 files ≥ 456 lines carry ~65% of the code**, 8 files are ≤ 58
lines, and the middle is thin. The TypeScript side is the opposite and healthier — one 1048-line
outlier (`App.tsx`, 84 hook calls) and everything else between 56 and 366.

The 8–11 line `mod.rs` files are doc + `pub mod` with **no `pub use`**. They are module manifests,
not barrels, and satisfy the rule as-is. They stay.

---

## 1. Target tree

Only directories that change are shown. The rule applied throughout: a directory is earned by
having ≥ 2 real files; `mod.rs` always carries content; no `pub use` hubs.

```
src/
├─ foundation/        + disk.rs        (disk_space, today duplicated in guard.rs AND local.rs)
├─ model/             − vclock.rs      (deleted — §6)
├─ fs/vfs/
│  ├─ remote.rs       NEW  ~120  shared backend helpers (~145 dup lines collapse)
│  ├─ smb/            {mod, windows, macos, unsupported, umount}   ← 3 cfg-selected siblings
│  ├─ sftp/           {mod, conn, staged, attrs}
│  └─ ftp/            {mod, conn, listing, staged}
├─ store/
│  ├─ hashcache.rs    NEW  ← from pipeline/scan.rs:80-120,187-201
│  ├─ mtimefix.rs     NEW  ← from pipeline/scan.rs:122-185   (deletes the apply→scan edge)
│  ├─ migrate.rs      NEW  ← from store/settings.rs:134-260
│  └─ version/        {schema, write, restore}
├─ obs/
│  ├─ runlog/         {mod, record, recorder, index, retention}
│  └─ progress/       {mod, sink, ctl, phase}
├─ transfer/pack/     {format, build, extract}   (orchestration half → run/)
├─ pipeline/
│  ├─ compare/        {mod, keys, moves, mirror, sync, attrs, winnames, case, conflict, evidence}
│  ├─ apply/          {mod, copy, mutate, dir, delta, preserve, schedule, ledger}
│  ├─ scan/           {mod, local, remote, digest}
│  ├─ guard/          {mod, caps, marker, roots, space, stats, ratio}
│  └─ filter.rs       stays flat (~345 lines once presets leave)
├─ job/               {mod, shape, rigor, opts, store, migrate, sample, territory}
├─ run/               {mod, roots, archive, local/{mod,compare,apply}, remote/{mod,link,compare,apply,delta}}
├─ cli/               {mod, args, commands/{run,jobs,pipeline,peer,logs,store,remote}}
└─ main.rs            ~35 lines

src-tauri/src/        {main ~60, dto, bridge, state, cmd/{mod,jobs,edit,run,results,logs,shell}}

typescript/
├─ core/              + grouping.ts stats.ts runevent.ts prefs.ts jobsummary.ts overview.ts
├─ ui/state/          {useJobs, usePlanSession, useViewState}
├─ ui/actions/        {useJobMutations, useCompareRun, useWatch}
├─ ui/platform/       {useDragDrop, useCompareProgress, useAppShortcuts}
└─ ui/components/     {shell/, job/, plan/, primitives/}
```

`transfer/remote.rs` (97 lines) stays flat — it is `lib.rs:33`'s own cited example of the
single-file-domain rule.

---

## 2. Relocation ledger (the user's rule 4, both directions)

### Out of where it is

| Code | From | To | Why |
|---|---|---|---|
| hash cache (56) + mtime-fix store (64) | `pipeline/scan.rs` | `store/hashcache.rs`, `store/mtimefix.rs` | persistent on-disk state in an L2 engine. **Deletes the only `apply→scan` edge** (2 lines) |
| `init_worker_pool` + `lower_thread_priority` (39) | `pipeline/scan.rs` | shell startup | never called by any pipeline code — only `main.rs:499` and `src-tauri:912` |
| junk presets (130) | `pipeline/filter.rs` | `job/` | every consumer is L3/L4; `PathFilter::build_full` never touches them |
| `disk_space` (38, ×2 copies) | `guard.rs` + `vfs/local.rs` | `foundation/disk.rs` | `local.rs:343-345` documents the duplication and blames the layering; `foundation` is the layer that resolves it |
| capability report (293) | `pipeline/guard.rs` | `pipeline/guard/caps.rs` | 43% of the file, announced nowhere in its header, shares no type with the three gates |
| log-dir migration (127) | `store/settings.rs` | `store/migrate.rs` | a cross-volume directory mover inside a config module |
| `apply_pack`'s pipeline invocation | `transfer/pack.rs:421-437` | `run/pack.rs` | L2 wire-format module calling the L2 execution engine = orchestration |
| `describe_root` (33) | `run.rs:117-149` | `cli/commands/remote.rs` | text rendering in L3 for exactly one shell |
| `civil_from_days`, `stamp`, CSV `esc`, `json_token`, `norm_root` (~64) | `src-tauri/src/main.rs` | `foundation`/`model` | the file header says "IPC orchestration only" |
| rigor ladder copy (20) | `src/main.rs:694-713` | *delete* | third copy of `Job::rigor_resolved`, silently missing `escalate`/`verify_writes` |
| `NameRules` (32) | `fs/vfs/mod.rs` | `model/table.rs` | it is table vocabulary — its serialized form is `Header.name_rules` |
| `Credentials`/`CredentialProvider` | `fs/vfs/mod.rs` | `fs/vfs/cred.rs` | the trait sits apart from its only impl |
| `RowSpec` | `ui/hooks/useVirtualRows.ts` | `core/grouping.ts` | imported *back up* into `App.tsx:21` — inverted dependency |
| `filterSummary`/`configPills` (23), Overview aggregation (31) | `ui/components/` | `core/` | pure functions, no JSX |
| `useJunkPresets` | `components/JunkPresets.tsx` | `ui/hooks/` | a data hook exported from a component file |

### Into where it belongs

| Code | To | Why |
|---|---|---|
| rdelta recipe build/apply (~80, **4 implementations**) | `model/chunk.rs` | it already hosts `RecipeStep`, moved there to break this exact cycle. `version.rs:352-375` re-implements it a fourth time *in a test* because there is no function to call |
| shared backend helpers (~145) | `fs/vfs/remote.rs` | 11 duplicated blocks across sftp/ftp/smb — and 3 hide real divergences (`abs("")` returns `""` vs `"/"`; two different timeout messages for one condition) |
| `write_jsonl` / `read_jsonl` | `model/` | 5 write sites, 7 read sites, header-then-lines repeated ~4× verbatim |
| `hash_file` / `hash_bytes` | `foundation` or `fs` | ~15 open-coded blake3 hasher constructions |
| `dirs::data_dir()` | `foundation/dirs.rs` | `trash_root()` and `cache_dir()` are structurally identical LOCALAPPDATA/HOME derivations — the exact failure `dirs.rs:6-13` exists to end |
| one `tallyOps()` | `core/stats.ts` | replaces two divergent tallies, **fixing the `chmod` bug** |
| one `reduceStages()` + `PHASE_LABEL` | `core/runevent.ts` | ~140 duplicated lines between the two windows, including *two different rate algorithms* |

---

## 3. Phases

Each phase ends green on all four checks. Phases 1–2 are independently valuable and can land
without committing to the rest.

### Phase 0 — Preconditions

1. **Coordinate the in-flight frontend work.** 28 files uncommitted (+1730/−1054): a forms/sheets
   + CSS pass that replaced `ContextMenu.tsx` with `ui.tsx` primitives. It is coherent (tests and
   typecheck pass). **Phases 6–7 must not start until it lands.** Rust phases 1–5 do not touch it.
2. **Characterization tests where coverage is zero, before touching anything.** This is the
   inversion that dictates phase order:

   | Unprotected | Lines | Tests |
   |---|---:|---:|
   | `transfer/pack.rs` — incl. `apply_pack`'s remote-side security gates | 456 | **0** |
   | `src/main.rs` (CLI) | 1101 | **0** |
   | `model/{table,event,plan,chunk}.rs` — the on-disk + IPC contract | 518 | **0** |
   | `fs/vfs/local.rs` — the backend every lane funnels through | 380 | **0** |
   | `fs/lock.rs` — the mutual-exclusion protocol | 134 | **0** |
   | `run.rs` remote lane | 416 | **0** |

   `apply_pack` runs on the *remote* machine and gates package version, plan hash, `is_safe_rel`
   on every path, and delta blob/base/result hashes. All untested. Add `tests/` integration
   coverage here first — this is the highest-risk block in the repo.
3. **Decide `fake://` and `vclock`** (§6).

### Phase 1 — Subtraction and adoption (compiler-verified, no structure change)

Highest value per unit of risk. Nothing moves; things disappear or get repointed.

- Delete the dead list in §0.2 (~1,175 lines including `vclock`, or ~460 if `vclock` is gated).
- Repoint the 5 unwired `foundation` helpers; delete `runlog.rs:98-120` and `src-tauri:514-581`
  outright, plus their two duplicated tests.
- **Fix `human_bytes` in TS** — align `format.ts` to KiB/MiB/GiB. User-visible.
- Fix the one barrel: `vfs/mod.rs:40-43`. Line 41 is dead; line 43 launders `model::table::EntryKind`
  through `fs::vfs`, which is the "a barrel erases who depends on whom" case `lib.rs:36` names.
- Make `remote::shq`/`shq_for` private.
- Document the two L0→L1 macro edges in `fs/lock.rs` and `fs/vfs/local.rs` headers, and state the
  granularity of the Tarjan claim in `lib.rs`.

### Phase 2 — Relocation (moves only, no splitting)

Everything in §2. Ends with the `apply→scan` edge gone, `foundation` actually load-bearing, and
`src-tauri` down to IPC.

**Includes the presentation extraction** (§0.4): the 35 sub-shell `println!` sites return data
instead. This is what makes Phase 4's transport router possible — until the library stops printing,
the two shells cannot share one code path. Behavior-preserving; the CLI does the rendering it
already does, just at the top.

### Phase 3 — Split the pipeline

`compare/`, `apply/`, `scan/`, `guard/` per §1. `filter.rs` stays flat once presets leave.

Two notes that change the shape:
- `compare.rs` is a **1043-line** production file (656 are tests); the split is driven by the ten
  passes inside `compare()`, not the raw count. Its test module is already banner-sectioned along
  exactly those seams.
- `apply.rs`'s 14 tests all drive `apply()` against real temp trees — they are integration tests,
  not unit tests. Move them to `tests/apply.rs`; `apply/mod.rs` then lands at ~265 rather than ~675.

### Phase 4 — Orchestration and shells

`run/`, `cli/`, `src-tauri/src/`, `job/` per §1.

The payoff is one **transport router** (`run::compare()` / `preflight()` / `apply()`) absorbing the
`remote_host.is_some()` branch, killing 6 duplicated sites; one run-log bracket instead of 3; one op
filter instead of 4; one startup bootstrap instead of 2.

`run.rs` splits almost perfectly in half by transport — local 424 lines, remote 416 — which is why
this is a clean cut rather than a judgement call.

### Phase 5 — VFS

`smb/` first: its three `mod platform` blocks are already cfg-selected siblings behind one identical
signature, sharing nothing else. Textbook. Then `sftp/` (`connect_and_open` is already a free `async fn`
taking no `&self` — it moves verbatim), then `fs/vfs/remote.rs`.

Then **wire sftp/ftp/smb into `conformance::run_all`**. Today no protocol backend runs the contract
suite; `conformance.rs:2-3` calls it an "admission ticket" that has never been collected. The three
backends that talk to untrusted remote machines carry the thinnest tests in the layer (3.8%, 5.2%,
7.4%), and none exercises a protocol path.

### Phase 6 — Frontend *(only after the in-flight work lands)*

- `App.tsx` 1048 → **~165** via `ui/state/`, `ui/actions/`, `ui/platform/` + the `core/` extractions.
  With one `SessionContext` for the four values 6+ components need, ~120. Do **not** put
  `plan`/`checked`/`flipped` in context — they change per row.
- `components/` → `shell/` `job/` `plan/` `primitives/`. This partition is derived from the actual
  import edges, not imposed: **`job/` has zero cross-group edges** — all six members' edges are internal.
- **Adopt the 6 unused generated DTOs.** `PlanDto`, `ProgressEvent`, `SamePage`, `SameRow`, `Phase`,
  `LogLevel` are generated and ignored while hand-written twins are maintained beside them.
  `plan.ts:11-12` admits it in a comment. The `run-progress` event is modelled **three times**
  (`CmpEv`, `RunEv`, and the generated union), and the contract has already drifted — `purpose`
  exists on neither generated type. Add `purpose` on the Rust side so it regenerates.
- Fix the two state bugs: the `chmod` tally divergence, and the 1:N target switch that leaves
  `maskHit`/`chips`/`ovFilter`/`sort` stale (7 reset sites, no two agree → one `resetPlan()`).
- Make `ED_GROUPS` a union type. The group literals are currently consistent, but the contract is a
  bare `string` across three files with nothing to catch drift.
- Add an ESLint config — there is none, so `react-hooks/exhaustive-deps` has never run and the one
  `eslint-disable` comment in the tree is decorative. 4 dependency-array escapes exist.

### Phase 7 — CSS and build

- `tokens.css` + `base.css` + `progress.css` + ~13 area files. Tokens are cleanly separable (lines
  24-126 are custom properties only). **Relocate `.stagerow` and `.chkline` first** — both are
  defined in one window's section and consumed by the other.
- Guard: `Graph.tsx:28-31` reads three tokens off `documentElement` every canvas frame at 10 Hz, with
  light-theme literal fallbacks. Dropping tokens from that entry fails *silently* in dark mode.
- Constraint that survives any split: Tauri's CSP nonce blocks inline `<style>`/`style=""` wholesale.
  All dynamic styling must stay on the CSSOM path (the code currently complies).
- **Fix the misnamed bundle.** `dist/assets/zoom.js` is **220 KB** — React + react-dom + lucide + all
  of `core/` — named after `core/zoom.ts` (35 lines) because Rollup picked a representative module.
  The entire stylesheet ships as `zoom.css`. One line of `manualChunks` or `chunkFileNames`. Zero risk,
  independent of everything else.

---

## 4. What does not change

Worth stating so the refactor doesn't damage it:

- **`foundation/`'s zero-in-crate-dependency contract** — verified clean; the only `crate::` in the
  directory is the doc comment stating the rule.
- **`core/` is React-free** — verified. Keep it that way. (Two Tauri imports, and
  `ipc.ts:47-62 withCapsConsent` calling `window.confirm()`, are the exceptions to fix.)
- **The `mask_match` pattern** — glob semantics live in Rust and the frontend cannot own them.
  This is the model the byte/duration/percent duplications should be converted to.
- **`conformance.rs`** as a shared backend contract suite — the layer's best asset.
- **The doc comments.** This codebase's comments carry design rationale (the `minmax(0,1fr)` trap,
  contrast ratios, why uppercase not lowercase folding). Per the comment policy, those are KEEP
  categories. Audit them for *drift* during moves — several already describe superseded machinery
  (`read_dir_names`' "backends override this" is false; `guard.rs:611` has its doc and
  `#[allow]` attached to the wrong function; `logging` tags still say `"preflight"`).

---

## 5. Sequencing

| Phase | Content | Risk | Guarded by | Net lines |
|---|---|---|---|---|
| 0 | Coordinate WIP; characterization tests for the 6 unprotected blocks | none | — | +~600 test |
| 1 | Delete dead code, adopt `foundation`, fix the barrel, fix `human_bytes` | **low** | compiler + 231 tests | **−1,200** |
| 2 | Relocation; presentation out of L0–L2 | medium | compiler + tests | ~−250 |
| 3 | Split pipeline | medium | 1,555 pipeline test lines | ~0 |
| 4 | Split run / cli / src-tauri / job; transport router | medium | Phase 0 tests | ~−150 |
| 5 | Split VFS; wire backends into conformance | medium | conformance suite | ~−145 |
| 6 | Frontend decomposition; adopt generated DTOs | **high** | typecheck + manual | ~−200 |
| 7 | CSS split; fix bundle naming | low | visual | ~0 |

Phase 1 alone removes roughly 1,200 lines and fixes a user-visible unit bug. Phase 5's conformance
wiring is the largest single **correctness** gain. Phase 6 is the highest risk and must wait.

---

## 6. Decisions taken (2026-07-28)

**1. `fake://` — deleted entirely.** Not gated. `fs/vfs/fake.rs`, the `RootSpec::Fake` variant, the
`vfs::open` arm, the `job` validation arm and the `src-tauri` label are all gone; `fake://` now takes
the unknown-scheme path like any other typo.

*Known cost, accepted:* `scan_root` dispatches on `vfs.as_local()`, and `LocalVfs` returns `Some`, so
local roots take the `scan_impl` lane. `scan_vfs` (241 lines, the lane every protocol backend uses)
therefore has **no coverage at all** now, and the `degraded_caps_demand_consent` test — which guarded
"refuse to run degraded without `--accept-caps`" — is gone with it. The three `sftpish`/`ftpish`
conformance profiles went too, leaving sftp/ftp/smb with no contract coverage.
**Phase 0 restores this with a minimal `#[cfg(test)]`-only VFS double that cannot ship.**

**2. `vclock.rs` — deleted.** 362 lines, zero callers. README's two references now record it as
history rather than as a present-tense prerequisite.

**3. Scope — all seven phases**, in the order below.

---

## 7. What was done (2026-07-28)

All seven phases executed, each on its own branch and merged with `--no-ff`. Every phase ended
green: `cargo check --workspace --all-targets` with **zero warnings**, `cargo test --workspace`,
`npx tsc --noEmit`.

**Tests: 231 → 233**, and the composition changed more than the count. 27 tests were deleted with
the dead modules they belonged to (`vclock` held all 11 of `model/`'s); 29 were added under code
that had none.

**Files ≥ 400 lines: 16 → 11. Rust files: 50 → 99.** Line total is roughly flat (~19.3k) — this
was redistribution, not deletion, apart from Phase 1.

| Phase | Result |
|---|---|
| 1 | −1,523 lines. `vclock.rs` and `fake.rs` deleted, the crate's only barrel removed, three `civil_from_days` collapsed to one, `humanSize` aligned with Rust |
| 0 | `MemVfs` (test-only) restores the `scan_vfs` lane and the degraded-caps consent gate; `tests/pack_roundtrip.rs` covers the remote trust boundary; `model/` gets round-trip tests |
| 2 | `store/{hashcache,mtimefix,migrate}`, `foundation/disk`, `job/{junk,rigor}`, `boot`. The `apply → scan` edge is gone |
| 3 | `pipeline/{compare,apply,scan,guard}/` — 4 files → 24 |
| 4 | Transport router: six duplicated branches → zero. `run/`, `cli/`, `src-tauri/{dto,bridge,state,cmd/}`. `main.rs` 1102 → 29 |
| 5 | `smb/` per-platform, `sftp/` and `ftp/` give up their stream and staged types |
| 6-7 | Generated DTOs adopted where safe (unconsumed 9 → 5); bundle renamed `vendor`/`shared` |

### Deliberately not done

**The frontend decomposition.** A second session has been live in `App.tsx`, `filter.ts`,
`plan.ts`, `styles.css`, `FilterBar`, `PlanTable`, `Toolbar` and `useVirtualRows` throughout, and
independently created `core/grouping.ts` moving `RowSpec` out of `useVirtualRows` — the same
relocation §3 Phase 6 proposed. Doing the rest would have overwritten live work. Still open:
`App.tsx` (1047, 84 hooks), the `components/` regrouping, the CSS split, the `chmod` tally
divergence, the 1:N stale-state bug, `ED_GROUPS` as a union, and an ESLint config.

**Two long functions, named rather than left implicit.** `compare()` is 549 lines whose ten passes
accumulate into shared state — splitting them is a control-flow change, not a move. `run_cli`'s
558-line dispatch would need each arm lifted into a function. Both are honest single
responsibilities; neither is a file-organization problem.

**Remaining ≥ 400-line files** are mostly one thing each: a `Vfs` impl (`sftp` 639, `ftp` 518), a
dispatch (`cli` 630), a walker (`scan/local` 511). `obs/runlog.rs` (594) and `job/mod.rs` (712)
are the two that would still repay a split.
