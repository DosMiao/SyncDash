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
│   ├─ store/                 L1  settings · trash · version
│   ├─ obs/                   L1  progress · logging · runlog
│   ├─ pipeline/              L2  scan · compare · apply · filter · guard
│   ├─ transfer/              L2  remote · pack
│   ├─ job/                   L3  the Job schema · territory
│   ├─ run.rs                 L3  the orchestrator
│   └─ main.rs                L4  CLI bin
├─ src-tauri/                 L4  Tauri v2 desktop shell: thin IPC layer
├─ typescript/                frontend (Vite + React 19; styling is a hand-written CSS token layer, no UI library)
│   ├─ core/                  framework-free domain + IPC; components never call invoke() directly
│   │   └─ types/generated/   wire types ts-rs generates from the Rust structs — do not hand-edit
│   ├─ ui/                    main window: App.tsx owns session state, components/ render, hooks/ isolate effects
│   ├─ progress/              the run sub-window (own entry point; run state lives in a ref, see its header)
│   └─ styles.css             the whole design token layer — every size and color in the app resolves here
├─ Script/gen-types.mjs       type generation entry point (`npm run gen:types`)
├─ index.html                 main-window entry point (the sub-window's is progress.html)
├─ dist/                      frontend build output — deliberately committed to git (no node on Mac, see "Build")
├─ builder.bat                Windows build menu (Dev / Desktop / CLI / All, plus run and kill/unlock/clean)
└─ builder.command            Mac build script (pure cargo)
```

Dependencies point **downward only**, and this is checked rather than asserted: Tarjan over the
comment-stripped sources reports no strongly-connected component larger than one and no edge pointing
up the ladder.

Two shape rules: a **single-file domain stays flat at its parent** (`transfer/remote.rs`, `run.rs`) —
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
syncdash scan <root> [--out t.jsonl] [--no-hash] [--force-rehash] [--symlinks-direct] [--progress] [--junk ids] [--exclude PHRASE]...
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
schema = 2                 # job-file schema; a file without it is migrated on load (junk presets -> exclude)
mode = "sync"              # mirror | sync | enrich
source = 'D:\Code\Utilities\flight'
target = '\\192.168.0.115\xuanbomiao\Code\Utilities\flight'
archive = 'C:\Users\xuanb\AppData\Roaming\syncdash\archive\flight.jsonl'   # sync mode
# include = ['*']                       # FFS filter-syntax allowlist (empty = everything)
# exclude = ['*/big_temp/', '*/*.log']  # FFS syntax. THE WHOLE exclude policy besides this tool's own metadata:
#                                       # junk presets write their patterns straight into this list, so it always
#                                       # reads as what runs. `syncdash junk` prints them.
# rigor = "standard"                    # quick | fast (sampled digest) | standard | paranoid (see "Rigor levels")
# case_sensitive = false                # case-insensitive by default (the NTFS/APFS default behavior)
# no_hash = false
```

`run <job> --apply` **refreshes the archive automatically** when it succeeds (0 errors) in sync mode (conflicting
paths are dropped from the archive, so the next run reports the conflict again instead of having it silently arbitrated).

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
**Compare** (runs in the background via spawn_blocking, the UI never freezes) → the diff table: checkbox + colored
action badge (`→ copy` / `← copy` / `⇢ move` / `✕ delete` / `⚡ conflict`) + path / from / size / reason →
stats bar (items / selected / bytes to transfer / conflicts) → **Synchronize** applies the checked rows and
**re-compares automatically** afterwards to verify convergence. conflict/note rows are locked and cannot be
checked. Frontend: Vite + React 19, styled by the hand-written token layer in `typescript/styles.css` — no UI
or styling library. Every size and color resolves through that one file, with a hard floor of 11px type and
4.5:1 contrast; the whole window scales through the webview's own zoom (Ctrl +/-/0), not a font knob, so
borders and layout scale with the text.

Added in v0.3.2: **per-row direction flip** (click the action badge to toggle; the semantics are precomputed by the
core's `reverse_op`: copy↔delete are inverses, update swaps sides; a flipped row gets a dashed border and a tinted
background), **filter chips** (all/copy/update/move/delete/conflict, live counts, 0-item chips dimmed — GitDash
style), **search box** (substring over path/from/reason), **pre-sync confirmation sheet** (per-category counts +
bytes, deletions highlighted in red), **keyboard shortcuts** (Ctrl/⌘+R compare, Ctrl/⌘+F search, Enter
synchronize, Esc close overlay), **immersive Mac title bar**.

v0.9 "Progress & Polish" (behavior parameters cross-checked against the FFS 14.10 source; plan in plans/ffs-ui):
- **Standalone progress sub-window** (the same one FFS has): during compare it shows live item/byte counts for
  both sides being scanned; during apply, dual cumulative graphs (bytes + items), a 4s sliding-window rate, a 60s
  sliding-window ETA, a big percentage `(bytesDone+itemsDone)/(bytesTotal+itemsTotal)`, done/remaining, the
  current file, the percentage in the window title + Windows taskbar progress.
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
- **Watch mode** (timed rescans, not inotify): a second-scale `watch_interval_secs` gives "near real-time"; the
  hash cache means an unchanged tree costs only the walk. Desktop Watch toggle (countdown + notify on differences /
  auto-apply); CLI `run --watch [--interval N] [--auto-apply]`.
- **Remote jobs now take the real remote pipeline in the GUI** (they used to fall silently into the local
  pipeline, re-hashing over UNC an order of magnitude slower); ssh badge in the sidebar.
- **The old egui UI has been retired and deleted** (removed as agreed once Tauri reached feature parity; the bare
  CLI and `syncdash gui` now launch the desktop app, and the workspace release build went ~2.5min → ~56s).
- Engine foundation: a unified `ProgressEvent` stream (PhaseStart/Totals/Progress/Error/Paused/Resumed/Summary,
  with throttling owned by the sink); **five-phase apply**, with the Copy/Update phases parallelized (`parallel`,
  default 4; 2-4 streams saturate the uplink on SMB; Update with delta enabled keeps a serial lane to avoid memory
  spikes); DeleteDir is deepest-first within its class.

v0.9.2 "FFS parity" (catching up on the batch of buttons FFS users press every day and we had none of):
- **Directory picker / drag-and-drop / path history / path health check**: the editor's two roots no longer have
  to be typed by hand — a browse button (tauri-plugin-dialog), drop a folder in and it fills (Tauri v2 swallows
  the HTML5 drop event, so we go through `onDragDropEvent` + physical-pixel hit testing), a `<datalist>` that
  remembers the last 12 roots, and `inspect_paths` validating live (exists / is a directory / the two roots are
  the same / one nests inside the other / `.syncdash-root` present or not).
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
  a single byte). **Click-to-sort headers** (path/action/both sizes/both mtimes) — sorting and tree grouping are
  mutually exclusive, because grouping relies on the invariant that rows in the same directory are contiguous in
  the plan.
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
- **Identical-items panel** (that "22,631" button along the bottom of FFS): lists the files judged identical on
  both sides, paged 300 at a time, with its own path filter; the data source is the two snapshots the last compare
  left in memory — **no rescan**. Rows whose content matches but whose timestamps drift more than 2s across the
  two sides get the target time marked orange (a common artifact of FAT/SMB granularity). Single-slot cache,
  overwritten when you switch jobs or re-compare; it works for remote jobs too (the remote snapshot is a complete
  table pulled back over ssh).
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
- Hashing is BLAKE3 (mmap+rayon, multi-core) with a cache: if `(path,size,mtime)` is unchanged the previous result
  is reused; the cache lives in the local user directory and never pollutes the scanned directory.

## Semantics of the three modes

| | Add (missing on the far side) | Change (the two sides differ) | Delete (extra on the far side) | Move |
|---|---|---|---|---|
| **mirror** (source=master) | ✅ Fill it in on target | ✅ master wins unconditionally | ✅ Delete target's extras | ✅ In-place move on target |
| **sync** (bidirectional) | ✅ Fill in both ways | archive attribution: one-sided change → propagate; both-sided change → **conflict** | Deletes only with an archive (to tell "deleted" from "added") | ✅ A one-sided move is replayed on the other side |
| **enrich** (add-only, never delete) | ✅ Fill it in on target | Updates only when source is strictly newer | ❌ Never | ❌ (a move contains a delete, which violates add-only) |

Equality test: if both sides have a hash → the hashes must match; otherwise sizes must be equal and
|Δmtime| ≤ 2s (FAT/SMB time granularity).

### Rigor levels (rigor) — a monotone ladder: each level actually reads more this scan

Design principle (the v0.9.3 refactor): **an "identical ✓" must be measured this scan, not remembered from
cache**. Cache exists at exactly one level, fast (and it says so); from standard up, every file is really read
every scan. Two cross-cutting reinforcements:

- **Divergence escalation** (fast/standard): when the sampled digests match but |Δmtime| > 2s, it does not call
  them equal — both sides escalate to a full hash and the verdict is redone. Verified on real hardware: 64 bytes
  changed outside the sampling window of a 400MB file, and the escalation rule caught it on the spot.
- **Verify after write** (on by default for standard/paranoid): the expected value is **the full blake3 of this
  copy's own stream** (the copy reads the whole file anyway, so hashing on the stream is free), re-read after it
  lands and compared; if it doesn't match, no rename. Deeply decoupled from scan evidence.

| Level | Actually read this scan | What "identical ✓" means | Move detection | Verify after write | Suited for |
|---|---|---|---|---|---|
| `quick` | 0 bytes | metadata measured this scan (size+mtime±2s) | ❌ | ❌ | structural sweeps |
| `fast` | the sampling windows of the changed surface only (cache accelerates the unchanged surface) | the changed surface measured, the unchanged surface remembered from cache | ✅ | ❌ | cloud drives / media libraries (placeholder files hydrate only three small segments) |
| `standard` (default) | **every file's sampling windows, no cache** | head/middle/tail of every file measured this scan | ✅ | ✅ | day to day |
| `paranoid` | every byte of every file | every byte measured this scan | ✅ | ✅ | first migration / annual cold-backup audit / suspect media |

Sampling window = size + 256KB each of head/middle/tail (<4MB is read in full; the `~` prefix keeps these
strictly isolated from full hashes in the cache).

**Threat coverage × detection latency** (the metadata/structure audits every level shares are not restated here —
existence, path normalization, type, size/mtime, symlink target, permission bits, illegal-name preflight, archive
attribution, apply gates):

| Threat | `quick` | `fast` | `standard` | `paranoid` |
|---|---|---|---|---|
| Ordinary modification (changes size/mtime) | immediately | immediately | immediately | immediately |
| Any change that touched mtime (including outside the sampling window) | immediately (metadata level only) | **immediately** (the escalation rule re-verifies in full) | **immediately** (escalation rule) | immediately |
| A rewrite that preserves size+mtime (timestomp) | never | immediately inside the sampling window; never outside | **measured every scan** inside the sampling window; never outside | immediately |
| Silent bitrot | never | never on the unchanged surface (cache) | **measured every scan** inside the sampling window; never outside | immediately |
| Transfer corruption | never | never | **immediately** (verify after write) | immediately |
| Move identity | delete+add | paired | paired | paired |

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
pass it with `--archive` next time** (v0.3 automates this step).

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
2. `ssh <host> syncdash scan <root>` — collect the table off stdout.
3. `compare` locally to produce the plan.
4. `syncdash pack plan.jsonl --out pkg.zip`: **the files to be written + the plan (including the delete list) + a
   hash of the data section + a hash of the plan**.
5. After transfer (scp or a shared drive, either works), `ssh <host> syncdash apply-pack pkg.zip`: verify both
   hashes first, then work through the plan step by step — dry-run by default here too.

Win↔Mac SSH is verified working (port 22 is open on the Mac; passwordless login just needs the public key written
into authorized_keys).

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

The remote pipeline (set `remote_host`/`remote_root`/`remote_exe` in the job): `run` automatically goes
ssh probe → **the remote scans its own disk** (no pulling data over UNC to hash it; an order of magnitude faster
on large territories) → compare locally → the target-side pack is delivered over ssh stdin → `apply-pack` on the
remote (with lock/trash/verify built in) → the source-side pull-back lands directly through the mounted path →
archive refresh. `gen-jobs --remote-host mac --remote-root-base /Users/xxx/Code` can generate remote-pipeline jobs
for every territory in one shot.

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
  within a single file) — queued for v0.3.

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
- [x] v0.3.x compare classification-matrix unit tests (the full archive-attribution matrix, 20 tests); two-phase parallel scan (rayon hashes whole files in parallel, splitting internally at ≥32MB); `compare::reverse_op` per-row direction flip (in egui, click the action badge to flip; the Tauri shell reuses the very same lib function)
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
- **The roadmap is fully complete**. The only remaining long-range direction is version-vector P2P (see
  "Multi-endpoint" — an explicit non-goal unless direct writes bypassing the hub appear).
  The mathematical groundwork was done and then removed: a complete version-vector implementation rewritten from
  the semantics of syncthing's `lib/protocol/vector.go` (`update` monotonicity, `merge` as least upper bound, and
  antisymmetry of the comparison relation all exhaustively verified) lived in `src/model/vclock.rs` through v0.9
  without ever being wired to archive attribution. It was deleted rather than kept as scaffolding — true N-way
  requires maintaining the vectors precisely after every apply and guaranteeing convergence, and that is v1.0's
  engineering. Recover it from git history when that work starts.

## Build

**Windows**: double-click `builder.bat` ([1] Dev HMR / [2] Desktop / [3] CLI / [4] All), or by hand:

```bash
npm run build && cargo build --release -p syncdash-desktop   # desktop app
cargo build --release -p syncdash                            # CLI
```

The menu carries a second row as well: `[R]` launches the desktop executable already built, `[5]` and `[6]` kill a
running instance and then wait for its file locks to clear, and `[7]` runs `cargo clean` over the workspace — which
does **not** touch `dist/`, that being a committed artifact the Mac cannot regenerate. Each phase is timed and each
artifact is printed as a ctrl-clickable link with its size.

**Every build frees the binary it is about to write first**, which is not politeness but a requirement: cargo links
straight over `target\release\syncdash-desktop.exe`, so an app left open ends the build at the link step with
`Access is denied. (os error 5)`. The two binaries are handled differently, though. The desktop shell is killed
without asking — it is a viewer over the library and relaunches in a second. A running `syncdash.exe` is only
reported, and killing it takes an explicit *y*: the CLI can be halfway through an `apply`, holding the root
heartbeat lock and writing files, and a failed build is much the cheaper of the two outcomes.

**Mac (no node required)**: the `dist/` frontend output ships with git and Tauri embeds it at compile time, so
pure cargo produces the complete GUI:

```bash
bash builder.command     # = cargo build --release -p syncdash-desktop -p syncdash
```

After changing the frontend (typescript/, index.html): run `npm run build` once on Windows and commit dist/ along
with it; the Mac just pulls and rebuilds.

**Getting the repo onto the Mac** (when there is no GitHub remote): if the Mac has the D drive mounted,
`git clone /Volumes/D-AnonyD/Code/Utilities/SyncDash ~/Code/Utilities/SyncDash`;
if not, push it in reverse from Windows: `git -c windows.appendAtomically=false -c core.autocrlf=false clone D:\Code\Utilities\SyncDash '\\192.168.0.115\xuanbomiao\Code\Utilities\SyncDash'`
(macOS's SMB does not support git's atomic append writes, so `windows.appendAtomically` must be turned off;
`autocrlf=false` preserves the LF endings of .command/.sh).
