# SyncDash (`syncdash`)

A table-driven multi-endpoint file sync CLI. Three stages: **scan (produce a table) → compare (diff the tables into a plan) → apply (execute the plan)**,
and every stage takes and emits human-readable JSONL tables — the table is the interface, and ssh pipes, archives and audits all use the same format.

Why not just keep using FFS / rsync / Unison:

| Pain point | Existing tools | SyncDash |
|---|---|---|
| A moved file is treated as delete+add | FFS (across machines), rsync | ✅ (hash,size) pairing produces a real `move` |
| Multi-endpoint (1 master to N slaves, three-way tables) | Unsupported, or barely workable | ✅ Tables are first-class: N tables, N plans |
| Remote "pack it over, the far side verifies then executes" | None | v0.4: zip + manifest + dual hash + far-side verification |
| Compare evidence is inspectable and archivable | Black box | ✅ Snapshot tables and plan tables are all JSONL files |

## Architecture (v0.3, modeled on AlexQuant Desktop)

```
SyncDash/
├─ src/                       syncdash core library + CLI bin — one directory per layer
│   ├─ foundation/            L0  fmt · time · path · text · names · dirs   (zero in-crate deps)
│   ├─ model/                 L0  plan · event · table · chunk              (vocabulary, no I/O)
│   ├─ fs/                    L0  staged (atomic write) · lock · vfs/ (one root = one backend)
│   ├─ store/                 L1  settings · trash · version · hashcache · mtimefix · migrate
│   ├─ obs/                   L1  progress · logging · runlog
│   ├─ pipeline/              L2  scan/ (local · vfs) · compare/ · apply/ · guard/ · filter
│   ├─ transfer/              L2  peer (the ssh lane) · pack
│   ├─ job/                   L3  the Job schema · territory · junk presets · rigor
│   ├─ run/                   L3  the orchestrator: local · peer · roots · archive, behind one transport router
│   ├─ boot.rs                L3  process startup: worker pool, settings, progress sink (both shells)
│   ├─ cli/                   L4  args (the --help contract) · dispatch
│   └─ main.rs                L4  CLI bin (29 lines: parse, dispatch, exit)
├─ src-tauri/                 L4  Tauri v2 desktop shell: dto · bridge · state · cmd/
├─ typescript/                frontend (Vite + React 19; styling is a hand-written CSS token layer, no UI library)
│   ├─ core/                  framework-free domain + IPC; components never call invoke() directly
│   │   └─ types/generated/   wire types ts-rs generates from the Rust structs — do not hand-edit
│   ├─ ui/                    main window: App.tsx owns session state, components/ render, hooks/ isolate effects
│   ├─ progress/              the run sub-window (own entry point; run state lives in a ref, see its header)
│   └─ styles.css             the whole design token layer — every size and color in the app resolves here
│                             (values follow GitHub Primer; changing one means re-running the contrast audit)
├─ Script/gen-types.mjs       type generation entry point (`npm run gen:types`)
├─ index.html                 main-window entry point (the sub-window's is progress.html)
├─ dist/                      frontend build output — deliberately committed to git (no node on Mac, see "Build")
├─ builder.bat                Windows build menu (Dev / Desktop / CLI / All, plus run and kill/unlock/clean)
└─ builder.command            Mac build script (pure cargo)
```

Dependencies point **downward only**, and this is checked rather than asserted: Tarjan over the
comment-stripped **files** reports no strongly-connected component larger than one. Two things that
reading `use crate::` will not show, both accepted rather than engineered away: the `log_*!` macros are
`#[macro_export]` and expand to `$crate::obs::logging::emit`, so `fs::lock` and `fs::vfs::local` carry an
edge to L1 that no `use` declares (each header says why); and counting those, the *directory* graph has
one cycle, `fs → obs → store → fs`. Being able to log from anywhere is worth more than an acyclic
directory graph — but it is a cycle, and the file-level claim is the one that holds unqualified.

One word to keep straight, because it used to name two opposite things. A **peer** job is one the far
side's own syncdash executes (`peer://`, `run::peer`, `transfer::peer`); everything else runs in this
process however distant its roots — an `sftp://` root is reached over a network but read and written
*here*, down `pipeline::scan::vfs`. "Remote" answered both questions and so answered neither; it
survives only in the run log's stored `kind` strings, where changing it would make existing history
read differently.

Two shape rules: a **single-file domain stays flat at its parent** (`boot.rs`, `pipeline/filter.rs`) —
only a multi-file domain earns a directory, because nesting for its own sake only lengthens paths. And
**no re-export hubs**: every `mod.rs` carries real content and callers write the full path
(`foundation::fmt::human_bytes`), since a barrel erases who depends on whom, which is the one thing
this layering exists to keep visible.

`model` holds vocabulary while the engines that produce it live in `pipeline` and `obs`. That split is
what makes the graph acyclic: `store::version` and `obs::runlog` both persist ops and `store::settings`
needs `LogLevel`, so leaving those types inside their engines forced service modules to reach up into
the domain layer for a struct definition.

Language: code, identifiers, comments and UI text are all English (same convention as AlexQuant Desktop).
Two exceptions are deliberate and must survive future sweeps — `foundation/text.rs` keeps two CJK strings as
**test fixtures** (`safe_host("主机") == "--"` asserts that non-ASCII host characters each collapse to `-`;
an ASCII input would map to itself and assert nothing), and `typescript/styles.css` keeps the
`"Microsoft YaHei UI", "PingFang SC", "Noto Sans CJK SC"` fallbacks in `--font-ui`, which is what renders CJK
**file paths** from the user's filesystem in the diff table. They sit at the *end* of that stack deliberately:
Office installs Segoe UI on macOS and every Mac has PingFang, so a CJK face placed early would capture Latin
text on the other platform. Spelling is en-US.

## Commands

```bash
syncdash jobs                                    # List the job configs
syncdash run <job> [--apply] [--i-know]          # End to end: scan both sides → compare → gates → (--apply) apply → refresh archive
syncdash run --all | --prefix cs- [--apply]      # Batch run (the engine behind hub-and-spoke multi-endpoint)
syncdash territories <root>                      # List .ffs-sync territories
syncdash gen-jobs <root> --target-root R [--junk ids] [--force] [--remote-host mac --remote-root-base /Users/x/Code]
syncdash junk [--patterns ids]                   # The junk exclude presets, and the exact patterns each writes into a job's exclude
syncdash gui                                     # Launch the desktop app (same as running syncdash-desktop directly)
syncdash probe                                   # Local environment as JSON (remote probing: ssh the far side and run this)
syncdash scan <root> [--out t.jsonl] [--no-hash] [--force-rehash] [--symlinks-direct] [--progress] [--junk ids] [--include PHRASE]... [--exclude PHRASE]...
syncdash compare --source a.jsonl --target b.jsonl \
    [--mode mirror|sync|enrich] [--archive last.jsonl] [--resolve-newer] [--case-sensitive] [--out plan.jsonl]
syncdash apply plan.jsonl [--apply] [--verify] [--delta] [--no-fsync] [--source-root R] [--target-root R] [-v]
syncdash mark <root> [--job NAME]                # Drop the .syncdash-root mount-point marker (pairs with require_marker)
syncdash trash runs|find <pat>|restore <pat> --into R|prune   # Local trash: inspect / recover / clean up
syncdash logs list [job] [--limit N]             # Run overview (interrupted runs included)
syncdash logs show <run-id> [--errors|--items|--plan]   # The four artifacts of one run
syncdash logs prune [--keep-days N] [--max-total-mb N]  # Clean up per the retention policy
syncdash logs dir                                # Where the log directory / settings file live
syncdash pack plan.jsonl --out pkg.tar           # Pack the target-side ops (payload + plan + dual-hash manifest)
syncdash apply-pack pkg.tar [--apply] [-v]       # Far side: verify hash → extract → apply (lock/trash/verify all included)
syncdash-desktop                                 # Tauri desktop app (the main GUI)
```

## Job config (modeled on FFS's "one .ffs_gui, one config")

One TOML per job, kept in `%APPDATA%\syncdash\jobs\` (mac: `~/.config/syncdash/jobs/`):

```toml
schema = 3                 # job-file schema; older files migrate on load (junk presets -> exclude, then remote_host -> peer://)
job_id = "0123456789abcdef0123456789abcdef"  # registry-assigned; survives rename, changes after delete/recreate; not in config_revision
mode = "sync"              # mirror | sync | enrich
source = 'D:\Code\Utilities\flight'
target = '\\192.168.0.115\xuanbomiao\Code\Utilities\flight'
archive = 'C:\Users\xuanb\AppData\Roaming\syncdash\archive\flight.jsonl'   # sync mode
# include = ['*']                       # FFS filter-syntax allowlist (empty = everything)
# exclude = ['*/big_temp/', '*/*.log']  # FFS syntax. THE WHOLE exclude policy besides this tool's own metadata:
#                                       # junk presets write their patterns straight into this list, so it always
#                                       # reads as what runs. `syncdash junk` prints them.
# rigor = "standard"                    # quick | fast | balanced | standard | paranoid (see "Rigor levels")
# case_sensitive = false                # case-insensitive by default (the NTFS/APFS default behavior)
# no_hash = false
```

`run <job> --apply` **refreshes the archive automatically** when it succeeds (0 errors) in sync mode (conflicting
paths are dropped from the archive, so the next run reports the conflict again instead of having it silently arbitrated).

The registry assigns `job_id` on first registered load/save without changing the job's semantic
`config_revision`. Renaming moves that identity with the TOML; deleting and recreating the same name creates a
different identity, so callers can distinguish replacement from an ordinary edit. Desktop save returns one typed
effect (`created`, `updated`, `renamed`, or `no_op`) and delete returns `deleted`; save/delete are serialized against
the same run-entry gate as Compare/Synchronize, so a configuration cannot change between a run's idle check and its
job load.

## Logging (v0.10)

One unified diagnostic path: the engine's narration, per-item failures and per-item apply results all ride
`progress::ProgressSink`, the event bus that already existed, with a file sink hung off the end. **No tracing/log
crate** — stacking on another facade would only give the same thing two exits.

```
<log_dir>/                                  # defaults to <config>/logs, changeable in the settings
├─ runs.jsonl                               # index: one line per run
├─ app.jsonl                                # events outside any run (startup/cleanup/migration/settings errors)
└─ 20260727-002612-demo-apply/              # one directory per apply (compare only writes the index line)
   ├─ summary.json    run summary (written with finished:false at start, overwritten at finish)
   ├─ plan.jsonl      the plan: what this run **intended** to do
   ├─ run.jsonl       event stream: narration, phase boundaries, errors
   ├─ errors.jsonl    error list: Error plus any Log at warning level or above
   └─ items.jsonl     apply list: the **actual outcome** of each op, one per line (ok/failed/kept/cancelled)
```

Three things are what matter in this release:

- **Streamed to disk**. Events are written as they arrive (flushed every 64 lines, or on an error / phase
  boundary). v0.9 wrote it all in one shot at `finish()`, so killing the process lost the whole thing. Now,
  hard-kill a 4000-file sync and `plan.jsonl` is complete, `items.jsonl` retains the part that finished, and
  `finished:false` in `summary.json` says it never ran to the end.
- **Plan and apply kept separate**. What v0.9 wrote into the detail file was the plan ops handed to apply — which
  one succeeded, which failed, which was kept because the directory was non-empty: not a word. Diff the two
  files and you know which ops never got their turn.
- **The desktop app can finally actually see**. `src-tauri` is a windowed build with no console; the 32
  `eprintln!` sites in the library ("remote schema mismatch", "delta disabled", "stale lock taken over",
  "source-side op(s) skipped") might as well have said nothing. They are `Log` events now and land in the log panel.

**Not a single `println!` was touched**: `remote::ssh_capture` reads the snapshot table off the stdout of a remote
`syncdash scan`, and stdout is a data cable, not a log.

Settings live in `<config>/settings.toml` (editable from the ⚙ in the desktop log panel):

```toml
log_dir = ""             # empty = the default <config>/logs; on save, the old directory is migrated wholesale
level = "info"           # info | warn | error
keep_days = 30           # 0 = no age-based cleanup
max_total_mb = 512       # 0 = unlimited (the apply list records everything; this is its seatbelt)
log_compare = "summary"  # summary = write only the index line, no directory | off
                         # no full tier: watch mode every 30s = 2880 runs a day
mirror_stderr = true     # the CLI also mirrors verbatim to stderr (terminal experience word-for-word as before)
```

## GUI (desktop app `syncdash-desktop`, Tauri v2)

An FFS-shaped dark two-pane UI: the job list on the left (mode badges: mirror blue / sync green / enrich orange) →
**Compare** (runs in the background via spawn_blocking, the UI never freezes) → the diff table, laid out
symmetrically around what happens: checkbox + source path / size / time + **action** + target path / size / time +
reason. The action is coloured text with a direction arrow and a per-category glyph (`→ ⧉ copy`, `← 🗑 delete`,
`⚡ conflict`) — the same glyph and hue the matching filter chip and stats segment use, from one map in
`typescript/ui/icons.tsx` →
stats bar (items / selected / bytes to transfer / conflicts) → **Synchronize** applies the checked rows and
**re-compares automatically** afterwards to verify convergence. conflict/note rows are locked and cannot be
checked. Frontend: Vite + React 19, styled by the hand-written token layer in `typescript/styles.css` — no UI
or styling library. Every size and color resolves through that one file, with a hard floor of 11px type and
4.5:1 contrast; the whole window scales through the webview's own zoom (Ctrl +/-/0), not a font knob, so
borders and layout scale with the text. Both themes are defined there and follow the OS through
`prefers-color-scheme`; the values are GitHub Primer's, except where Primer's own hues miss the contrast
floor on the hover surface and take a darker step. Changing any color means re-running the audit against
`--bg`, `--bg-2` and `--bg-3` — several hues clear the first and fail the third.

Added in v0.3.2: **per-row direction flip** (click the row's action to toggle; copy↔delete are inverses and update
swaps sides; the frontend derives that preview lazily, while preflight/apply send only row indices + flip flags and
the Rust core's `reverse_op` reconstructs the executable operation; a flipped row gets a tinted background and a
dashed outline on its direction arrow), **filter chips** (all/copy/update/move/delete/conflict, live counts, each
carrying its category's own hue and glyph, 0-item chips dimmed by token — GitDash
style), **search box** (substring over path/from/reason), **pre-sync confirmation sheet** (per-category counts +
bytes, deletions highlighted in red), **keyboard shortcuts** (Ctrl/⌘+R compare, Ctrl/⌘+F search, Enter
synchronize, Esc close overlay), **immersive Mac title bar**.

v0.9 "Progress & Polish" (behavior parameters cross-checked against the FFS 14.10 source; plan in plans/ffs-ui):
- **Phase-aware progress**: compare shows both concurrent scans inline; synchronize uses a standalone window with
  phase-local item/byte counters, dual cumulative graphs, a 4s sliding-window rate, a 60s sliding-window ETA,
  done/remaining, and the current file. Active work is capped at 99% and switches to “Finalizing” after its measured
  counters are exhausted; only the explicit terminal event may show 100%. Each interactive run reserves a launch ID
  and the window acknowledges that exact ID after its listeners are ready, so fast or duplicate starts cannot lose or
  cross-wire their opening phase, totals, or rejection. Closing before the first event cancels that reservation. Cross-volume
  preservation (notably an external mirror copying old content into central trash) joins the byte total and uses the
  same chunk-level pause/cancel checkpoints; archive-refresh failures become run errors rather than a green Summary.
- **Pause/Continue** (the engine spin-pauses: elapsed time freezes while the RootLock heartbeat keeps beating, so
  the machine on the other end won't mistake it for an abandoned lock), **Stop = cooperative cancel** (responds
  between chunks; atomic writes guarantee the destination never holds a half-written file and leaves zero
  `.syncdash.tmp.*` residue), **error accumulation panel** (errors do not halt the run — FFS semantics; stderr is
  lost in a windowed build, so errors and warnings all ride the event stream), **Auto-close** and
  **When finished** (sleep/shutdown, with a 10-second cancellable countdown).
- **Overview aggregation sidebar** (collapsible, to the left of the diff table): aggregates items/bytes/share bars
  by top-level directory, click to filter the diff table, the second level expands lazily; **iconified stats bar**
  (zeros greyed out, non-zeros bold and colored).
- **Run logs**: every real apply writes a `logs/runs.jsonl` index entry plus per-run detail (the finalized op list
  + accumulated errors); the sidebar job rows show **last sync** (a result-colored dot + relative time, turning
  red past 7 days); the GUI log panel can review history and detail; CLI `syncdash history [job] [--prune-days N]`.
- **Job editor** (full-field form: mode / roots / rigor level / filters / safety gates / the three remote fields /
  watch, with create, edit and delete-with-confirmation).
- **AutoScan while SyncDash is open**: the desktop backend, not the webview, owns its lifecycle and binds every
  trigger to one exact job identity, revision and target. A pure rename rebinds the displayed name without resetting
  the watcher generation; delete/recreate cannot inherit the old ticket even if the name and configuration match.
  On macOS, two local roots use FSEvents change hints plus periodic
  full verification; remote roots and platforms without a native adapter report an explicit interval-polling
  fallback. A change event is never treated as a complete snapshot: every trigger still runs Compare, and a failed
  or stale ticket does not advance the durable native cursor. `watch_interval_secs` is the polling / maximum full
  verification interval; CLI `run --watch [--interval N] [--auto-apply]` remains a foreground timed loop.
- **Remote jobs now take the real remote pipeline in the GUI** (they used to fall silently into the local
  pipeline, re-hashing over UNC an order of magnitude slower); ssh badge in the sidebar.
- **The old egui UI has been retired and deleted** (removed as agreed once Tauri reached feature parity; the bare
  CLI and `syncdash gui` now launch the desktop app, and the workspace release build went ~2.5min → ~56s).
- Engine foundation: a unified `ProgressEvent` stream (PhaseStart/Totals/Progress/PhaseEnd/Error/Paused/Resumed/Summary,
  with progress throttled independently per phase and every completed/failed/cancelled boundary delivered); **five-phase apply**, with the Copy/Update phases parallelized (`parallel`,
  default 4; 2-4 streams saturate the uplink on SMB; Update with delta enabled keeps a serial lane to avoid memory
  spikes); DeleteDir is deepest-first within its class.

v0.9.2 "FFS parity" (catching up on the batch of buttons FFS users press every day and we had none of):
- **Directory picker / drag-and-drop / path history / path health check**: the editor's two roots no longer have
  to be typed by hand — a browse button (tauri-plugin-dialog), drop a folder in and it fills (Tauri v2 swallows
  the HTML5 drop event, so we go through `onDragDropEvent` + physical-pixel hit testing), a `<datalist>` that
  remembers the last 12 roots, and `inspect_paths` validating local roots live (exists / is a directory / the two
  roots are the same / one nests inside the other / `.syncdash-root` present or not). Network and peer phrases are
  explicitly **deferred**, not painted as healthy: the editor does not open credentials or a transport merely to
  color an input, and Compare reports the actual connection/readiness result when it owns a cancellable run.
- **⇄ Swap**: one click swaps them inside the editor; the toolbar swap **writes back to the TOML** and invalidates
  the current plan (with undo). What FFS swaps is the config held in memory; our jobs are named files on disk —
  without persisting, the two roots in the plan header would say something different from the job file, and both
  the run log and the archive refresh would point the wrong way.
- **Diff-table context menu**: reveal in Explorer (`reveal`, without going through a shell) / copy the full or
  relative path / exclude this type `*/*.ext` / exclude this directory `/rel/dir/` (written back to the job's
  exclude, with undo) / reverse this row / check only this item / uncheck this directory.
- **Both-side size + modified-time columns**, tinting the newer side green when the two mtimes differ by more than
  2s — "which side is newer" previously had to be guessed out of reason, and conflict rows didn't even carry
  size/mtime. The data comes from a new read-only evidence layer in the core library, `compare::evidence()` (it
  shares `norm_key`/`files_equal` with `compare()`, and the `Op` struct and on-disk plan format did not change by
  a single byte). **Every column sorts** — both side paths, action, both sizes, both mtimes, reason — and sorting
  works *inside* the tree: rows order within each directory while the directories order by an aggregate of the same
  key (summed bytes for a size, newest member for a time, lowest action rank, the dir name itself for a path).
  `core/grouping.ts` owns display order for exactly this reason: it used to be split between the row sort and the
  group builder, which is why the two had to be mutually exclusive. A column the responsive layout folds away hands
  its sort key to the surviving column on the same side, so no key is ever unreachable.
- **Status-bar counts**: `Showing X / Y · Z hidden, not run · Scanned A ⇄ B · Identical K` (FFS's "Showing 481 of 23,112").
  `source_entries`/`target_entries` had been sitting in the plan header all along.
- **Funnel filter** (applies to the current result, no rescan): name mask (FFS syntax) + size range + time span.
  Mask matching goes back to Rust's `filter::mask_hits` — the frontend never writes a second glob of its own,
  because only then does a mask you tried out in the UI behave the same once written into the job's exclude. A
  button at the bottom of the panel **promotes** a temporary mask into a persistent job exclude.
- ⚠ **The view is the action set**: rows hidden by the funnel / search / category chips **will not be applied**
  (FFS semantics). This fixes a quiet trap in the old behavior — filtering with the search box used to leave
  hidden-but-still-checked rows to go through with Synchronize anyway. The confirmation sheet now spells out
  "N items hidden by filters, not applied", and the stats bar switches to counting checked ∩ visible to match.
- **Bounded last-successful compare repository**: the desktop retains the eight most recently used completed
  `PlanDto` review sessions, including which rows are checked and which directions were reversed, keyed by stable
  `job_id`, selected target and captured configuration revision. Thus compare A → compare B → return to A restores A's result,
  and different targets of one job retain independent reviews. Only a successful compare publishes a result, so a
  failed or cancelled attempt cannot evict the last good review. LRU eviction bounds the accumulated snapshot/plan
  memory; if the frontend copy was evicted while Rust still owns the authenticated entry, selection restores it over
  IPC without rescanning. The repository is deliberately process-local: after a desktop restart the filesystem may
  have changed while SyncDash was closed, so the app asks for a fresh Compare instead of presenting persisted evidence
  as current. The revision is a canonical digest of the job contents, so a content-changing job-file mutation changes
  it and invalidates only that job/revision; unrelated jobs and targets remain available. A no-op editor save keeps the
  result because its effective revision is unchanged; a pure rename preserves and relabels that result, while deleting
  and recreating the same name produces a new identity that cannot see it. Every Compare attempt refreshes the job row after reading an
  externally edited TOML — including a failed or cancelled attempt, so a removed target or deleted job cannot leave a
  ghost selection that fails forever. Compare and Apply use a structured review protocol rather than caller-owned
  consent flags: the backend probes current health/capabilities, binds the job ID, revision, target, retained Compare
  owner, plan digest and normalized row decisions into an expiring one-use authorization, then recomputes those facts
  immediately before reserving execution. The webview sends only that token to the execution command; it cannot send a
  plan, acknowledge a different capability report, or replay the token. Session grants are process-local and scoped to
  the exact job/revision/target/capability digest; unattended Apply still requires a fresh, server-reconstructed action
  set and refuses health warnings. Apply also exposes a typed mutation boundary: a proven pre-write rejection keeps the
  retained Compare result available for another review, while any path that may have started a write invalidates it.
  An empty selection is rejected before a run is reserved; in particular, AutoScan will never turn a conflict/note-only
  result into an archive-changing zero-operation apply.
- **Identical-items panel** (that "22,631" button along the bottom of FFS): lists the files judged identical on
  both sides, paged 300 at a time, with its own path filter; the data source is the two snapshots the last compare
  left in memory — **no rescan**. Rows whose content matches but whose timestamps drift more than 2s across the
  two sides get the target time marked orange (a common artifact of FAT/SMB granularity). The retained snapshots follow
  the same bounded, target-aware repository contract, and their provenance includes the effective target, so two targets
  of one job cannot read each other's identical rows. It works for remote jobs too (the remote snapshot is a complete table pulled
  back over ssh).
- **CSV export**: exports the current view (including checked state and both-side size/time), escaping is done
  exactly once on the Rust side, UTF-8 **with a BOM** — without the BOM, Excel interprets it in the local code
  page and the whole column of Chinese paths turns to mojibake. The enum literals use serde's snake_case, the
  same source as the plan JSONL and the event stream.
- **Scheduled-task command** (our counterpart to FFS's "Save as batch job"): one click in the editor copies
  `schtasks /create ... syncdash run <job> --yes`. **It does not register the system scheduled task for you** —
  that is a system-settings-level action, and a human should press it themselves in an admin terminal.
- **Category chips became independent toggles** (so you can look at "added + deleted" alone), F5 / F9 = compare /
  synchronize, the Compare and Synchronize buttons gained subtitles spelling out the current rigor and mode, and
  the gear beside them jumps to the matching group in the editor.

- `scan` writes to stdout by default (ssh-friendly: `ssh mac syncdash scan ~/Data > mac.jsonl`).
- `apply` is **dry-run by default**; only `--apply` touches anything. Deleted or overwritten files go first into
  the local `%LOCALAPPDATA%\syncdash\trash\<timestamp>\` (mac: `~/.cache/syncdash/trash/...`), never destroyed in place.
  The common apply boundary rejects absolute, drive-prefixed and traversal-shaped operation paths (including a
  move's `from`) before opening either backend, so even a hand-authored plan cannot escape its two roots.
- Hashing is BLAKE3 with a cache: if `(path,size,mtime)` is unchanged the previous result
  is reused; the cache lives in the local user directory and never pollutes the scanned directory.
  VFS cache identities normalize only scheme and host; case-sensitive usernames and root paths stay
  distinct. Error-free scans prune absent rows inside the configured filter domain while retaining
  deliberately excluded rows; a walk error conservatively retains every unseen row. State I/O remains
  best-effort, but failures are emitted as warnings instead of silently turning every later scan cold.
  Files are **read, never memory-mapped** — a mapped page whose file was truncated or whose volume
  disappeared raises SIGBUS, which kills the process outright instead of returning an error, and in
  `apply` that leaves both root locks on disk. The multi-core gain is given up deliberately;
  parallelism is per-file, not intra-file. See the `src/lib.rs` header.

## Semantics of the three modes

| | Add (missing on the far side) | Change (the two sides differ) | Delete (extra on the far side) | Move |
|---|---|---|---|---|
| **mirror** (source=master) | ✅ Fill it in on target | ✅ master wins unconditionally | ✅ Delete target's extras | ✅ In-place move on target |
| **sync** (bidirectional) | ✅ Fill in both ways | archive attribution: one-sided change → propagate; both-sided change → **conflict** | Deletes only with an archive (to tell "deleted" from "added") | ✅ A one-sided move is replayed on the other side |
| **enrich** (add-only, never delete) | ✅ Fill it in on target | Updates only when source is strictly newer | ❌ Never | ❌ (a move contains a delete, which violates add-only) |

Equality test: if both sides have a hash → the hashes must match; otherwise sizes must be equal and
|Δmtime| ≤ 2s (FAT/SMB time granularity).

### Rigor levels (rigor) — explicit evidence and write-verification profiles

Design principle (the v0.9.3 refactor): **an "identical ✓" must be measured this scan, not remembered from
cache** when fresh evidence is requested. `fast` and `balanced` deliberately trade that property for a warm cache;
`balanced` adds verified writes, while `standard` and `paranoid` really read every file each scan. Two cross-cutting
reinforcements:

- **Divergence escalation** (fast/balanced/standard): when the sampled digests match but |Δmtime| > 2s, it does not call
  them equal — both sides escalate to a full hash and the verdict is redone. Verified on real hardware: 64 bytes
  changed outside the sampling window of a 400MB file, and the escalation rule caught it on the spot.
- **Verify after write** (on by default for balanced/standard/paranoid): the expected value is **the full blake3 of this
  copy's own stream** (the copy reads the whole file anyway, so hashing on the stream is free), re-read after it
  lands and compared; if it doesn't match, no rename. Deeply decoupled from scan evidence.

| Level | Actually read this scan | What "identical ✓" means | Move detection | Verify after write | Suited for |
|---|---|---|---|---|---|
| `quick` | 0 bytes | metadata measured this scan (size+mtime±2s) | ❌ | ❌ | structural sweeps |
| `fast` | the sampling windows of the changed surface only (cache accelerates the unchanged surface) | the changed surface measured, the unchanged surface remembered from cache | ✅ | ❌ | cloud drives / media libraries (placeholder files hydrate only three small segments) |
| `balanced` | the sampling windows of the changed surface only (cache accelerates the unchanged surface) | the changed surface measured, the unchanged surface remembered from cache | ✅ | ✅ | frequent external-disk syncs with safe writes |
| `standard` (default) | **every file's sampling windows, no cache** | head/middle/tail of every file measured this scan | ✅ | ✅ | day to day |
| `paranoid` | every byte of every file | every byte measured this scan | ✅ | ✅ | first migration / annual cold-backup audit / suspect media |

Sampling window = size + 256KB each of head/middle/tail (<4MB is read in full; the `~` prefix keeps these
strictly isolated from full hashes in the cache).

**Threat coverage × detection latency** (the metadata/structure audits every level shares are not restated here —
existence, path normalization, type, size/mtime, symlink target, permission bits, illegal-name preflight, archive
attribution, apply gates):

| Threat | `quick` | `fast` | `balanced` | `standard` | `paranoid` |
|---|---|---|---|---|---|
| Ordinary modification (changes size/mtime) | immediately | immediately | immediately | immediately | immediately |
| Any change that touched mtime (including outside the sampling window) | immediately (metadata level only) | **immediately** (the escalation rule re-verifies in full) | **immediately** (escalation rule) | **immediately** (escalation rule) | immediately |
| A rewrite that preserves size+mtime (timestomp) | never | immediately inside the sampling window; never outside | immediately inside the sampling window; never outside | **measured every scan** inside the sampling window; never outside | immediately |
| Silent bitrot | never | never on the unchanged surface (cache) | never on the unchanged surface (cache) | **measured every scan** inside the sampling window; never outside | immediately |
| Transfer corruption | never | never | **immediately** (verify after write) | **immediately** (verify after write) | immediately |
| Move identity | delete+add | paired | paired | paired | paired |

### Cross-platform correctness (v0.2.2)

- **Unicode normalization**: Mac (HFS+ forces NFD; APFS preserves the written form but matches
  normalization-insensitively) and Windows/Linux (NFC by convention) hand you different byte sequences for the
  same name. Compare keys are uniformly normalized to NFC — `café`(NFC) and `café`(NFD) are judged the same file;
  **on-disk I/O always uses each side's own original spelling, and never rewrites the other side's form the way
  Syncthing does** (it has a track record of turning NFC into NFD and breaking references).
- **Case**: NTFS/APFS are case-insensitive by default → compare keys fold case by default (set
  `case_sensitive = true` in the job to turn folding off; with it off, a case-only rename gets paired up by move
  detection into a single rename). Normalization collisions on the same side (NFD/NFC twins, case twins) → a Note
  reports them and whichever appeared first is kept, never silently merged.
- **Windows illegal-name preflight**: any path about to be created on the Windows side is checked first for
  reserved device names (CON/AUX/NUL/COM1-9/LPT1-9), illegal characters (`<>:"|?*`, control characters) and
  trailing dots/spaces → marked `Conflict("illegal-on-windows")` right at the **plan stage**, instead of letting
  apply blow up halfway through.
- **File attributes**: unix mode (the exec bit and friends) is already recorded in the snapshot table (the `mode`
  field); SMB can't carry it, so the v0.4 pack mode is responsible for restoring it. mtime is explicitly written
  back after a copy so the next compare's equality test holds.

### Versioning (v0.8, optional: `versioning = true`)

Once enabled, deleted or overwritten files no longer go into the local trash but into **that root's own
`.version_syncDash/`** — history travels with the data, so both machines can see it over SMB and both can restore
from it:

```
<root>/.version_syncDash/
  index.jsonl                version index (id, time, host, op count, saved count, bytes)
  <id>/plan.jsonl            the instruction list this run executed (audit)
  <id>/manifest.json         saved entries: whole|rdelta, each hash, original mtime/mode
  <id>/files/<rel>           original content stored whole (small files, deleted files)
  <id>/rdelta/<rel>          FastCDC reverse patch (overwritten files ≥4MB: old file = blocks the new file already has + a blob of blocks unique to the old)
```

- `syncdash versions <root> [--prune N]` — list / prune version history
- `syncdash restore <root> --version <id> [--file rel]... [--apply]` — recover (dry-run by default;
  rdelta requires the current file to match the recorded new_hash, and the reassembled result is verified against
  old_hash; the current content it displaces is kept in a side directory, not destroyed)
- Measured: a 5MB file overwritten costs the version store only 70,602 B (1.3%); after restore the SHA256 is
  bit-for-bit identical to the original
- Effective end to end: local apply, `apply-pack --versioning`, and the remote pipeline passes it through
  automatically; both scan and the FFS generator templates exclude `.version_syncDash/`, so the version store is
  never synced away as if it were data

### sync and the archive (the Unison approach)

"The far side doesn't have this file" naturally has two readings: **I added it** (copy it over) or **they deleted
it** (delete it here too)? The only reliable criterion is **an archive of the state at the last successful sync**
— the same idea as Unison's and FFS's databases:

- In the archive, absent on target, source unchanged → target deleted it → propagate the delete; source changed →
  **delete/modify conflict**
- Not in the archive → it's new → copy
- Both sides differ from the archive → **conflict**, never auto-arbitrated (unless `--resolve-newer` is explicit)

**With no archive, sync automatically degrades to safe mode**: it only fills in both directions, reports
differences as conflicts, and merely reports suspected moves (`possible-move-needs-archive`) — better to do less
than to do the wrong thing.
An archive is just an ordinary snapshot table: **after a successful sync, rescan either side and save it, then
pass it with `--archive` next time** (v0.3 automates this step). The refreshed table is written beside the archive,
flushed, and atomically replaced; a write failure or cancellation before commit leaves the previous archive intact.

**An archive is only usable against digests of its own evidence tier.** A sampled digest is `~`-prefixed
precisely so it can never compare equal to a full hash, so an archive gathered at one tier and read at another
would call every file over the 4 MB sampling floor "changed" — and a file the far side merely deleted would
surface as a delete/modify conflict, the one kind no `on_conflict` policy resolves. Two rules keep that from
happening: the archive is written at **the tier the comparison actually ran at** (the joint tier of both roots,
not the job's nominal `rigor` — a target that cannot do ranged reads forces both sides to full), and the archive
records that tier in its header. Reading one written at a different tier — after a `rigor` change, say — is
**refused with a warning** and the run falls back to safe mode above, rather than misread.

### Move detection (curing FFS's delete+add)

After comparing, the "to copy" list and the "to delete" list are paired on `(hash, size)`: the same content
disappearing from an old path and appearing at a new one → emit a `move` op (the same filename is preferred when
pairing, to handle whole-directory renames). Measured:

```
{"side":"target","action":"move","path":"moved/old_name.dat","from":"old_name.dat","reason":"move-detected-by-hash"}
```

One `rename` on target and it's done, zero re-transfer for large files. Scanning with `--no-hash` automatically
falls back to copy+delete (and gives up move detection).

## Remote mode (v0.4, implemented and verified on real machines)

1. `ssh <host> syncdash probe` — probe the far side: OS/arch/version/schema; if the binary isn't there it prints
   installation guidance (both machines have the Rust toolchain, so `cargo build --release` suffices; or just copy
   the binary across the shared drive).
2. `ssh <host> syncdash scan <root>` — collect the table off stdout. The far side is handed the job's
   **whole** filter (`--include` and `--exclude` both, plus `--junk none` so it adds no preset of its own)
   and the resolved rigor knobs. Both roots must be filtered by the same rule: a mask that binds only one
   side makes the other side's unlisted files look like deletions.
3. `compare` locally to produce the plan.
4. `syncdash pack plan.jsonl --out pkg.zip`: **the files to be written + the plan (including the delete list) + a
   hash of the data section + a hash of the plan**.
5. After transfer (scp or a shared drive, either works), `ssh <host> syncdash apply-pack pkg.zip`: verify both
   hashes first, then work through the plan step by step — dry-run by default here too.

Win↔Mac SSH is verified working (port 22 is open on the Mac; passwordless login just needs the public key written
into authorized_keys).

**Both ends must run the same build.** The probe compares `schema`, but nothing negotiates the *command line*:
stage 2 invokes the far side's `syncdash scan` with the flags this version knows about, so a far side that
predates one of them stops with `error: unexpected argument`. That is the intended failure — it is how adding
`--include` to the filter that crosses the link announces itself, instead of quietly filtering one root and
proposing deletions on the other. Upgrade the pair together.

## Multi-endpoint (settled in v0.6: hub-and-spoke)

**The supported topology is hub-and-spoke**: Win01 is the hub, and each spoke (the Mac, the E: cold backup, any
machine to come) gets one job (sync or mirror, whichever it needs, each with its own archive), with
`run --all` / `run --prefix cs-` running the lot in one keystroke.
Pairwise sync plus per-pair archives is mathematically equivalent to N-way propagation through the hub — entirely
correct for the reality of "one hub, many spokes".

**True P2P N-way (version vectors, the Syncthing approach) is explicitly a non-goal**, unless a need for
"spoke↔spoke direct writes that bypass the hub" ever appears — version vectors go in then, and the table format
has already left room for it architecturally (tables are first-class, and one table per endpoint follows
naturally).

The peer pipeline — a target named `peer://`, meaning the far side's own syncdash owns that root:

```
target = "peer://mac/Users/ben/Code|exe=~/bin/syncdash|mount=\mac\share\Code"
```

`run` then goes ssh probe → **the peer scans its own disk** (no pulling data over UNC to hash it; an order of
magnitude faster on large territories) → compare locally → the target-side pack is delivered over ssh → `apply-pack`
on the peer (with lock/trash/verify built in) → the source-side pull-back lands through the `mount=` path →
archive refresh. `gen-jobs --remote-host mac --remote-root-base /Users/xxx/Code` generates peer jobs for every
territory in one shot.

`mount=` is the pull direction and is optional: declared and reachable, the source-side ops run through it;
declared but unreachable, they are skipped naming the mount; undeclared, they are skipped saying how to enable it.
A job written before this grammar (schema ≤ 2, with `remote_host`/`remote_root`/`remote_exe`) migrates on load,
carrying both roots across so it keeps doing exactly what it did.

## Testing

Two executable contracts, one layer apart. `src/fs/vfs/conformance.rs` asks whether a **backend**
honours the `Vfs` trait — twelve checks, seeded through the write API, optional ones gated on
`caps()`. `src/run/e2e/` asks whether the **pipeline** honours the mode contracts above when running
on top of one: seed two roots, drift one, sync, then assert the plan, the bytes moved, what was
preserved, and the resulting tree. A move is proved three ways at once — the plan says `Move`,
`bytes_copied` is zero, and nothing reached trash — because the final tree looks identical whether
the tool renamed a file or copied and deleted it.

Both take the same shape (a factory returning fresh empty roots), so a live lane hands one closure
to both and gets the backend and the pipeline checked together.

**Skips are loud.** A case declares its `Need`s; an unmet need reports a skip and each lane pins its
skip set exactly, so a backend that quietly loses a capability turns a green run red. A LIST-only
FTP root, for instance, skips *every* case — it cannot hold a root lock without `set_mtime`, so it
is readable and never writable, and that is asserted rather than discovered.

```bash
cargo test --lib e2e                     # memory, local, sftp-shaped and ftp-shaped lanes
cargo test --workspace                   # everything, including tests/apply_ops.rs
```

Live lanes need a server and are `#[ignore]`d behind an env var:

```bash
SYNCDASH_E2E_SFTP_URL=sftp://user@host/scratch  cargo test --lib sftp_live_lane -- --ignored --nocapture
SYNCDASH_E2E_SMB_URL=smb://user@host/share/dir  cargo test --lib smb_live_lane  -- --ignored --nocapture
SYNCDASH_E2E_FTP_URL=ftp://anonymous@host:2121/ cargo test --lib ftp_live_lane  -- --ignored --nocapture
SYNCDASH_E2E_EXFAT_ROOT=E:\syncdash-e2e         cargo test --lib exfat_live_lane -- --ignored --nocapture
```

`smb://` additionally needs `syncdash cred set` first — a native SMB root cannot ride the session
login the way a `\\host\share` path can. Everything a live lane creates lives under the given root
and is removed before *and* after, so "fresh and empty" holds even if an earlier run died.

**What the live FTP lane established.** Two capabilities the backend declared turned out not to hold,
and a live server was the only thing that could show it:

- **`ranged_read` is now `No`, regardless of REST.** REST positions a transfer, but nothing in FTP
  ends one early and cleanly, and servers disagree about the reply. Both `ABOR` and closing the data
  socket left a response unconsumed, and the next command received it instead — a `stat` answering
  "350 Restarting at position 0" from two calls earlier. On one control connection that confusion
  cannot be contained, so the tier drops to full reads via the joint rule and says so. Sampled
  digests over FTP were a quiet evidence downgrade on exactly the large files sampling exists for.
- **The apply lane now clamps to `max_parallel_streams`.** It never did, so a four-wide default put
  a second transfer on a single-connection backend: `ftp://` copied one file per run and errored on
  every other. The trait had documented the clamp all along.

- **`staged_len` no longer asks the server.** It used to `SIZE` the temp file — while the upload was
  still open, so a server mid-STOR answered with however much it had flushed to disk. That is a race,
  not a length; over TLS the same 32 KiB write reconciled as 0, 16384 or 32768 bytes purely by
  timing. The writer knows what it wrote, so it says that, and whether the server really holds the
  bytes is asked after the transfer is finalized, where it can be answered honestly.

**`ftps://` connects and reads correctly** — the certificate verifies against the machine trust
store, which is the whole design working: a LAN server whose certificate its owner installed is
accepted, with no bypass flag anywhere. **Its writes are not yet trustworthy.** On the code path that
plain FTP moves a 6 MB file over without trouble, uploads arrive partial or empty, varying run to
run — a data-stream problem below this crate, not sync logic. Every occurrence was refused by the
post-rename size check, so nothing corrupt has ever been committed; the job fails loudly instead.
Treat `ftps://` as read-capable and write-unproven until that is chased down.

**Still open:** the conformance check `write_commit_visibility` stats and lists a directory while a
write is staged, which one control connection cannot answer. Closing it means a second control
connection per staged write; the pipeline does not need one, so it is recorded rather than papered
over. Everything else passes — 11 cases run, and the three sampling cases skip loudly against a
backend that honestly declares it cannot sample.

## Relationship to CodeSync (FFS)

Run them in parallel first: FFS keeps handling the day-to-day while SyncDash practices on the `.ffs-sync`
territories (the marker scan in `Update-CodeSyncConfig.ps1` could later be changed to emit syncdash's territory
list directly). It takes over once its behavior has earned trust.

## Borrowed from the FFS 14.10 source (`.docs/FreeFileSync_14.10_Source`, GPL — re-implemented from the semantics, no code copied)

- **path_filter.cpp → src/filter.rs**: the filter syntax is fully compatible with FFS — case-insensitive, takes
  both `/` and `\`, `*` crosses levels while `?` does not, a trailing `/` means directory, a leading `/` means
  root-relative, and `*/abc` also hits at root level; wildcard-free paths go through a constant-time hash-set
  lookup; the include side uses "the prefix might match" to decide whether to descend (the mechanism that lets an
  allowlist punch through intermediate directories). **An FFS exclude list can be pasted into a job config
  verbatim.**
- **dir_lock.cpp → src/lock.rs**: before apply, a `.syncdash.lock` is placed in both roots and the holding process
  refreshes its mtime as a heartbeat every 4s (visible to the other machine over SMB); finding someone else's lock
  with a live heartbeat → refuse to run; observing 12s with no heartbeat → declare it abandoned and take over.
  What this guards against is exactly the real risk of a two-machine setup: Win and Mac applying to the same
  directory at once.
- **algorithm.cpp (recorded in the design, no code changed)**: FFS's move detection relies on db anchors + file
  IDs + exact size/date (its comments stress that a tolerance must not enter a container predicate, since it
  breaks transitivity); pairing on content hash gives us stronger evidence, so we keep what we have. FFS collapses
  same-directory renames into a single displayed row — queued for v0.3.
- **parallel_scan.cpp**: parallel traversal of the directory tree (we currently do serial walkdir + rayon hashing
  across files, serial within one file) — queued for v0.3.

## Borrowed from the Syncthing source (`.docs/syncthing` @ `119d5e72`, MPL-2.0 — re-implemented from the semantics, no code copied)

The full side-by-side analysis is in [PLAN-syncthing-upgrade.md](PLAN-syncthing-upgrade.md) (every entry carries
real line numbers on both sides). What landed:

- **`lib/osutil/atomic.go` → [src/atomic.rs](src/atomic.rs)**: write a temp file in the same directory → fsync →
  rename. Same-volume rename is atomic, and an interruption leaves only the temp file. This one fixes a real
  data-loss path; it isn't fastidiousness.
- **`.stfolder` from `lib/config/folderconfiguration.go` → [src/preflight.rs](src/preflight.rs)**:
  the mount-point marker. It travels with the **data**, so if the drive isn't mounted the marker isn't there —
  this is the only reliable criterion for "the shared drive dropped". The marker itself must be excluded from the
  sync (otherwise empty directories would sprout markers out of nowhere), and syncthing likewise lists it as internal.
- **`CheckAvailableSpace` / `minDiskFree`** → a preflight that totals the sizes in the plan before writing.
- **`deleteDirOnDiskHandleChildren` (folder_sendrecv.go:1985)** → when a directory can't be deleted, report it by
  category, distinguishing "protected by a filter" / "can be deleted along with its children" / "a real error",
  no longer silently.
- **`conflictName` (:2219) / `WinsConflict` (bep_fileinfo.go:212)** → `.sync-conflict-<ts>-<host>` copies, newer
  mtime wins with the host name as a stable tie-break (we have no device id). **The default is still report-only**
  — not auto-arbitrating is SyncDash's founding principle.
- **`PreviousBlocksHash` (bep_fileinfo.go:200)** → the archive's multi-generation `prev` chain: one side merely
  being "a generation behind" is not a concurrent modification. This is the cheap approximation of version vectors
  available under the archive model.
- **`lib/fs/mtimefs.go`** → read the mtime back after setting it and record (ondisk, intended) in the local cache,
  so the ±2s tolerance is no longer the sole criterion (it matters most at `rigor = "quick"`).
- **`!` and `(?d)` from `lib/ignore/ignore.go`** → the filter's `!` exceptions and the `deletable` list, a strict
  superset of FFS syntax.
- **`lib/versioner/staggered.go:toRemove`** → staged thinning of the trash (dense recently, sparse further back),
  paired with `trash prune`.
- **The `Size == 0` guard at `lib/model/folder.go:930`** → empty files take no part in move pairing. All
  zero-length files share the same blake3, which used to get them paired into a pile of invented "renames".
- **`CaseConflictError` from `lib/fs/casefs.go`** → an on-disk name-collision preflight for case-sensitive mode
  (caught at the plan stage rather than blowing up at apply).
- **`shortcutFile` (:1253)** → `Action::Chmod`: no re-transfer when the content is identical and only the
  permission bits differ.
- **`lib/protocol/vector.go`**: a version-vector math core was ported and exhaustively verified, then **deleted**
  — it sat unreferenced for the whole of v0.9 while the config key that would have activated it was never built.
  True N-way is v1.0's convergence engineering; see "Multi-endpoint". The port is in git history if that day comes.

Explicitly **not** copied: the BEP protocol stack / TLS / device discovery / relaying / NAT traversal (going with
ssh + SMB is a deliberate simplification), a resident daemon (it would break the core promise of "dry-run by
default, nothing moves until a human clicks"), the index database (JSONL tables are readable, diffable and
pipeable — that's a selling point), encrypted folders, and filesystem watching (`watchaggregator`'s aggregation
strategy is worth reading, but adopting it means going resident).

## Algorithm research sources

- Unison's formal specification and archive model: [Balboa/Pierce, "What's in Unison?"](https://www.researchgate.net/publication/32205844_What's_in_Unison_A_Formal_Specification_and_Reference_Implementation_of_a_File_Synchronizer), [Unison: A File Synchronizer and Its Specification](https://link.springer.com/chapter/10.1007/3-540-45500-0_28), [Unison (Wikipedia)](https://en.wikipedia.org/wiki/Unison_(software))
- Version vectors for N-way sync: [File Synchronization with Vector Time Pairs](https://www.researchgate.net/publication/37991997_File_Synchronization_with_Vector_Time_Pairs), [Syncthing: Understanding Synchronization](https://docs.syncthing.net/users/syncing.html), [Syncthing conflict-detection improvement PR#10351](https://github.com/syncthing/syncthing/pull/10351)
- Algebraic filesystem reconciliation: [An Algebraic Approach to File Synchronization](https://www.cs.tufts.edu/~nr/pubs/sync.pdf)
- Delta transfer (v2 candidates): [The rsync algorithm](https://www.samba.org/rsync/tech_report/node2.html), [Dsync: Lightweight Delta Synchronization](https://lingfenghsiang.github.io/docs/DSync.pdf) (FastCDC content-defined chunking); compressed binary deltas such as .mph pay off too little, so they are deprioritized
- Unicode normalization and cross-platform filenames: [Explainer: Unicode, normalization and APFS](https://eclecticlight.co/2021/05/08/explainer-unicode-normalization-and-apfs/), [APFS's "Bag of Bytes" Filenames](https://mjtsai.com/blog/2017/03/24/apfss-bag-of-bytes-filenames/), [Apple APFS FAQ](https://developer.apple.com/library/archive/documentation/FileManagement/Conceptual/APFS_Guide/FAQ/FAQ.html), [File names & unicode normalization problems](https://nicolasbouliane.com/blog/unicode-normalization), [Windows file naming rules (Microsoft Learn)](https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file)

## Roadmap

- [x] v0.1 `scan` (table + hash cache), `compare` (mirror/sync/enrich + move detection + archive attribution), `apply` (local / mounted drives, dry-run by default, trash), `probe`
- [x] v0.2 job configs (jobs/*.toml), end-to-end `run`, GUI (Compare → check → Synchronize), automatic archive refresh after a successful sync
- [x] v0.2.1 FFS-syntax filters (fully compatible include/exclude, with unit tests) + a root heartbeat lock (prevents two machines applying concurrently, abandoned locks taken over automatically)
- [x] v0.2.2 rigor levels quick/standard/paranoid (verify after copy) + cross-platform correctness: NFC-normalized compare keys, case folding, Windows illegal-name preflight, unix mode recorded (with unit tests)
- [x] v0.3 Tauri v2 desktop shell (modeled on AlexQuant Desktop: Vite+TS frontend, dual-platform builder scripts, dist committed so the Mac builds with pure cargo and no node);
      rigor levels (quick/standard/paranoid: no hash / cached hash / full re-hash + verify after copy); NFC + case-folded compare keys; Windows illegal-path preflight
- [x] v0.3.x compare classification-matrix unit tests (the full archive-attribution matrix, 20 tests); two-phase parallel scan (rayon hashes whole files in parallel, splitting internally at ≥32MB); `compare::reverse_op` per-row direction flip (click the row's action to flip; the frontend previews it and the Tauri shell uses that same lib function to reconstruct the authenticated apply selection)
- [x] v0.4 remote: `pack` / `apply-pack` — a tar container (plan.jsonl + payload + trailing manifest), plan blake3 + per-file blake3 + a combined hash; nothing touches target until the whole staging area verifies; reuses apply's lock/trash/verify-after-copy; unix mode restored. **Pack on Win → ship over SMB → apply-pack on Mac → remote rescan shows 0 ops; the whole flow verified on real machines**
- [x] v0.5 `territories` / `gen-jobs`: scan for `.ffs-sync` markers and generate a `cs-<slug>.toml` per territory (sync mode + an automatic archive path) — the syncdash edition of the CodeSync generator, measured generating 11 territories; runs in parallel with FFS, the moment to switch left to the user
- [x] v0.6 `run --all`/`--prefix`; the end-to-end ssh remote pipeline (enabled just by setting remote_host in the job; verified on real machines: dry → apply → rerun 0 ops, symlinks included); symlink policies exclude/direct (compared by target, with apply creating/replacing/deleting the link itself); renames within the same parent directory are paired first (reason distinguishes rename from move); updating the Mac via a git bundle over SMB (the channel for when the mount is offline)
- [x] v0.7 all three "follow-up candidates" landed: **Windows as the remote** (the `recv` subcommand takes the package on raw Rust stdin, and the shell dialect is chosen from probe's os: doubled single quotes for PowerShell + a chcp 65001 prelude + `& 'exe'`; measured with the Mac driving Windows in reverse: an 8.4MB package landed over ssh stdin, apply-pack ran, rerun 0 ops); **FastCDC delta transfer** (16K/64K/256K, v2020; the remote's `chunks` produces the block table, updates ≥4MB send only the missing blocks + a reassembly recipe, with blake3 over blob, base and result; measured: 8MB file with 6KB changed transferred only 148KB, saving 98.2%); **GUI job editing** (egui: New/Edit/Delete full-field form + validation + delete confirmation; `config::save_job/delete_job` for the desktop shell to reuse)
- [x] v0.8 optional versioning: `.version_syncDash/` inside the root (plan instruction list + whole-file storage + FastCDC reverse patches) + the `versions`/`restore` commands; measured: an old version of a large file costs 1.3%, restore hashes match bit for bit
- [x] v0.9 **safety nets and capability catch-up against the syncthing source** (plan in [PLAN-syncthing-upgrade.md](PLAN-syncthing-upgrade.md), 78 tests):
      **atomic writes** (temp file in the same directory → fsync → rename, an interruption never leaves half a file at the final path — previously `fs::copy` wrote the target directly, so a broken Update left a truncated file to be propagated back to source on the next round);
      **the `.syncdash-root` mount-point marker + plan health check** (`require_marker` / `max_delete_ratio` / `--i-know`:
      a shared drive that never mounted, a mistyped filter, and source/target written backwards all look exactly alike, and this gate stops all three at once);
      **disk-space preflight** (`min_free_pct`); **categorized reporting of directory deletions** (no more swallowing them silently with `Err(_) => Ok(())`);
      **conflict copies** (`on_conflict = copy|newer`, `.sync-conflict-<ts>-<host>` + `max_conflicts`; still report-only by default);
      **multi-generation archive** (the `prev` chain: one side merely being "a generation behind" is no longer misreported as both-changed);
      **mtime read-back correction** (no longer leaning on the ±2s tolerance when FAT/SMB truncates timestamps);
      **filter `!` negation + `deletable`** (a superset of FFS syntax; pasted-in FFS rules behave unchanged);
      **trash retention** (`trash runs/find/restore/prune` + syncthing's staggered thinning algorithm);
      **local delta** (`delta`: large files patched by FastCDC block, worth it for SMB uploads);
      **`Action::Chmod`** (`sync_mode`: no re-transfer when the content is identical and only the permission bits differ);
      empty files are no longer mis-paired into "renames", ambiguous pairings honestly report their candidate count, case-collision preflight, scan progress (CLI `--progress` + a GUI progress bar)
- [x] v0.9 **"Progress & Polish" — catching up on FFS 14.10's during-apply experience** (90 tests): a unified progress/cancel/pause event-stream foundation;
      five-phase parallel apply (`parallel`); a standalone progress sub-window (dual cumulative graphs / rate / ETA / pause / stop / When-finished);
      the Overview aggregation sidebar + iconified stats bar; run logs + "last sync"; the full-field Tauri job editor; watch-mode timed rescans
      (`watch_interval_secs`/`--watch`); the real remote pipeline for remote jobs in the GUI; **egui retired and deleted** (see the GUI section above)
- [x] v0.10 one unified diagnostic path (the Logging section above), and **`smb://` became an in-process SMB2 client**.
      It used to hand the phrase to the operating system — a UNC path via `WNetAddConnection2W` on Windows, `mount_smbfs`
      subprocesses on macOS, a refusal on Linux — and delegate to the local backend on whatever path came back. It now
      speaks the protocol itself, which brings Linux with it and retires the mount orchestration and its `net umount` verb.
      **One user-visible trade, deliberate:** an `smb://` root now needs `syncdash cred set` first, because the SMB crate
      forbids unsafe code and so cannot reach SSPI to borrow this machine's login. `\\host\share` (and any smbfs mount
      point) still parses as a plain local path, still uses the login you already have, and still needs no configuration
      at all — that route did not go anywhere, it just stopped being the thing `smb://` silently meant.
- **The v0.1–v0.9 roadmap is fully complete**. The only remaining long-range direction is version-vector P2P (see
  "Multi-endpoint" — an explicit non-goal unless direct writes bypassing the hub appear).
  The mathematical groundwork was done and then removed: a complete version-vector implementation rewritten from
  the semantics of syncthing's `lib/protocol/vector.go` (`update` monotonicity, `merge` as least upper bound, and
  antisymmetry of the comparison relation all exhaustively verified) lived in `src/model/vclock.rs` through v0.9
  without ever being wired to archive attribution. It was deleted rather than kept as scaffolding — true N-way
  requires maintaining the vectors precisely after every apply and guaranteeing convergence, and that is v1.0's
  engineering. Recover it from git history when that work starts.

## Build

Double-click `builder.bat` on Windows or `builder.command` on macOS. Both are thin
launchers for the same Rust project Builder under `tools/builder/`, backed by the
shared core in the sibling `Experience/builder` repository. The complete `Code` tree
therefore keeps its normal relative layout.

The common Build row is `[D] Dev`, `[1] Dist`, `[2] Max`, `[3] Release`, and
`[4] Installer`. Enter `12`, `13`, `23`, or `123` to build those tiers
sequentially; there is no ambiguous `All` action. Every optimized tier packages the
desktop and matching CLI together under `target/builder-tiers/<tier>/`.

The Run row is `[V]` Dev on Windows plus `[S]` Dist, `[M]` Max, and `[R]` Release.
Utilities are `[K]` Kill, `[U]` Unlock, `[C]` Clean, `[O]` Doctor, `[I]` Info,
`[B]` Reveal, and `[Q]` Quit. macOS also has `[A] Install App`. Named commands are
stable for automation:

```text
builder.bat build dist
builder.command build cli
builder.bat build 123
builder.bat doctor
```

`build desktop` and `run desktop` remain legacy aliases for Dist. `build cli` remains
the explicit standalone-CLI shortcut and uses the Dist policy.

`--dry-run --host windows|macos` prints either platform's complete command plan
without building, killing, cleaning, installing, or launching anything. `clean` does
**not** touch `dist/`, because it is a committed artifact the Mac can consume without
Node. Each phase is timed and each artifact path and size is reported.

**Every build frees the binary it is about to write first**, which is not politeness but a requirement: cargo links
straight over `target\release\syncdash-desktop.exe`, so an app left open ends the build at the link step with
`Access is denied. (os error 5)`. The two binaries are handled differently, though. The desktop shell is killed
without asking — it is a viewer over the library and relaunches in a second. A running `syncdash.exe` is only
reported, and killing it takes an explicit *y*: the CLI can be halfway through an `apply`, holding the root
heartbeat lock and writing files, and a failed build is much the cheaper of the two outcomes.

**Mac Dist/Max/Release and standalone CLI (no Node required)**: the `dist/` frontend
output ships with git and Tauri embeds it at compile time, so those actions remain
pure Cargo:

```bash
bash builder.command build 123
```

After changing the frontend (typescript/, index.html): run `npm run build` once on Windows and commit dist/ along
with it; the Mac just pulls and rebuilds.

**Getting the repo onto the Mac** (when there is no GitHub remote): if the Mac has the D drive mounted,
`git clone /Volumes/D-AnonyD/Code/Utilities/SyncDash ~/Code/Utilities/SyncDash`;
if not, push it in reverse from Windows: `git -c windows.appendAtomically=false -c core.autocrlf=false clone D:\Code\Utilities\SyncDash '\\192.168.0.115\xuanbomiao\Code\Utilities\SyncDash'`
(macOS's SMB does not support git's atomic append writes, so `windows.appendAtomically` must be turned off;
`autocrlf=false` preserves the LF endings of .command/.sh).
