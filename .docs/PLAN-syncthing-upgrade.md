# SyncDash Upgrade Plan — Benchmarked Against the Syncthing Source

> Written: 2026-07-26
> Reference source: `.docs/syncthing` (`git clone --depth 1`, commit `119d5e72` / 2026-07-25, Go 1.25, MPL-2.0)
> This document only **borrows semantics**; it does not copy code (MPL-2.0 is not necessarily compatible with this project's license, and Go's concurrency model differs enough from Rust's that copying verbatim would be pointless).
>
> **Current status:** historical research and implementation snapshot. Paths, names, test counts,
> and architecture claims below describe the 2026-07-26 tree; `README.md` and `AGENTS.md` define
> the live implementation and validation contract.

## Implementation status (wrapped up 2026-07-26)

**P0 / P1 / P2 are all implemented, `cargo test --workspace` is 78/78 green, and end-to-end verification on real hardware passed.**

| Item | Status | Where it landed |
|---|---|---|
| P0-1 Atomic writes | ✅ | new [src/atomic.rs](src/atomic.rs) (`Staged` RAII) + [apply.rs](src/apply.rs) rewritten on every path |
| P0-2 Mount-point marker + plan health check | ✅ | new [src/preflight.rs](src/preflight.rs), `syncdash mark`, `require_marker` / `max_delete_ratio` / `--i-know` |
| P0-3 Disk space preflight | ✅ | [preflight.rs](src/preflight.rs) `disk_space` (Win: `GetDiskFreeSpaceExW`; unix: `statvfs`) + `min_free_pct` |
| P0-4 Classified reporting for directory deletion | ✅ | [apply.rs](src/apply.rs) `try_delete_dir` / `DirOutcome` |
| P1-1 Delta transfer | ✅ | the peer package path already had it in v0.7; this adds the local/mounted-drive path (`delta`, [apply.rs](src/apply.rs) `update_with_delta`) |
| P1-2 Conflict copies | ✅ | [compare.rs](src/compare.rs) `ConflictPolicy` / `conflict_name` / `max_conflicts` |
| P1-3 Multi-generation archive | ✅ | [table.rs](src/table.rs) `prev` + `roll_generations`, [compare.rs](src/compare.rs) `generation_of` |
| P1-4 mtime read-back correction | ✅ | [apply.rs](src/apply.rs) read-back + [scan.rs](src/scan.rs) `record_mtime_fixes` / `load_mtime_fixes` |
| P1-5 Filter `!` negation + deletable | ✅ | [filter.rs](src/filter.rs) `except` / `deletable` / `except_blocks_pruning` |
| P2-1 Version vectors | ✅ math core | new [src/vclock.rs](src/vclock.rs) (`Vector` / `Ordering` / node ID, exhaustive algebraic-property tests) |
| P2-2 Trash retention | ✅ | new [src/trash.rs](src/trash.rs): `trash runs\|find\|restore\|prune` + staggered thinning |
| P2-3 Case-collision preflight | ✅ | [compare.rs](src/compare.rs) folded collision → Conflict (case-only renames are not hit by mistake) |
| P2-4 Chmod op | ✅ | `Action::Chmod` + `sync_mode`; Copy/Update carry mode along |
| P2-5 Empty files excluded from pairing | ✅ | [compare.rs](src/compare.rs) `detect_moves` + `MovePair.candidates` ambiguity annotation |
| P2-6 Scan progress | ✅ | [scan.rs](src/scan.rs) `scan_with_progress`, CLI `--progress`, Tauri progress events |

**Scope of P2-1**: `vclock.rs` is a complete, well-tested version-vector implementation (covering `update`
monotonicity, `merge` as least upper bound, and exhaustive verification of the comparison relation's
antisymmetry). **But it has not yet taken over archive attribution** — true N-way requires precisely
maintaining every file's vector after each apply and guaranteeing convergence, which is v1.0 engineering
and out of scope this round. What ships this round is the math core plus node identity, along with P1-3,
the cheap approximation under the archive model (which already resolves the vast majority of
false-positive conflicts).

**Two corrections to the original plan** (the plan was written when the repo was at v0.5; by
implementation time it had reached v0.8):
- P1-1's claim of "whole-file copying" holds only for the **local/mounted-drive** path; the peer package
  pipeline already had FastCDC deltas in v0.7. This round fills in the former.
- P2-2's claim of "no versioning" is inaccurate: v0.8 already had a `versioning` mode
  (`.version_syncDash/` inside each root). What was genuinely missing is that **the default trash is
  never cleaned**, and that is what this round fills in.

Smoke testing also turned up a real bug the original plan did not foresee: the `.syncdash-root` marker
file gets synced across itself — so an unmounted empty directory would sprout a marker out of nowhere,
making the gate worthless. It has been added to `DEFAULT_EXCLUDES` (syncthing likewise lists `.stfolder`
as internal).

---

---

## 0. Method and reading scope

The syncthing files actually read through or read closely (every reference below points at real line
numbers inside `.docs/syncthing/`):

| Module | File | What we care about |
|---|---|---|
| Version vectors | `lib/protocol/vector.go` (all 329 lines) | The mathematical basis of true N-way sync |
| Conflict ruling | `lib/protocol/bep_fileinfo.go:190-229` | `InConflictWith` / `WinsConflict` / `PreviousBlocksHash` |
| Equality ruling | `lib/protocol/bep_fileinfo.go:454-540` | `FileInfoComparison` (optionally ignores perms/xattr/owner/blocks) |
| Blocking | `lib/scanner/blocks.go` (131 lines), `bep_fileinfo.go:92-102,395-412` | Block-level hashing, adaptive block size |
| Scanning | `lib/scanner/walk.go:300-400,615-624` | Temp-file cleanup, path normalization, descending into ignores |
| Pull engine | `lib/model/folder_sendrecv.go` (2235 lines; focus on 241-310 / 952-1250 / 1657-1710 / 1862-1906 / 1960-2090) | Temp files, block reuse, conflict copies, the directory-deletion safety net |
| Rename detection | `lib/model/folder.go:929-989` (`findRename`) | Local rename attribution during scanning |
| Atomic writes | `lib/osutil/atomic.go` (134 lines) | temp → fsync → rename |
| Versioning | `lib/versioner/{trashcan,staggered,simple,util}.go` | Trash retention, tiered thinning |
| Ignore syntax | `lib/ignore/ignore.go:359-400,500-560` | `!` negation, `(?i)`, `(?d)`, `#include` |
| Case-insensitive FS | `lib/fs/casefs.go:1-70` | `CaseConflictError`, resolving the real name before writing |
| mtime virtualization | `lib/fs/mtimefs.go:1-80` | Read back after writing on FAT/SMB, dual (ondisk, virtual) records |
| Health checks | `lib/config/folderconfiguration.go:37,63,160-196,236,360-375` | The `.stfolder` marker, `minDiskFree` |
| Watch aggregation | `lib/watchaggregator/aggregator.go:21-25,193-260` | Debouncing, degrading to a full scan at `maxFiles=512` |
| Receive-only mode | `lib/model/folder_recvonly.go:69-120` | `Revert` semantics |

---

## 1. First, be clear: these are not the same kind of thing

This determines what can be copied and what would be self-harm to copy.

| | Syncthing | SyncDash |
|---|---|---|
| Form | Resident daemon + web UI, P2P mesh | CLI + Tauri desktop, explicitly triggered |
| Transport | Its own protocol, BEP over TLS, with discovery/relay/NAT traversal | Rides on SMB mounts / ssh / tar packs |
| State | One index database per folder (sequence / version vectors / block tables) | JSONL snapshot tables + optional archive table |
| Topology | True N-way, any device is a peer | Mostly bilateral; 1 master to N slaves expanded by hand |
| Conflicts | Resolved automatically + `.sync-conflict-*` copies | Reported, never arbitrated automatically |
| Auditability | Requires reading the db / the logs | **The plan is a diffable text file** |
| When it acts | Continuously in the background; the user never sees the intermediate state | **Dry-run by default; nothing moves until a human clicks** |

**SyncDash's core selling point is "previewable, auditable, pipeable", and that cannot be sacrificed for
the sake of aligning with syncthing.**
So everything below that demands "resident", "implicit", or "black box" capability is demoted to an
optional mode, and never changes the default behavior.

---

## 2. Gap list

### P0 — correctness / data safety, to do immediately

#### P0-1. Writes are not atomic (**a real data-loss path**)

**syncthing**: every write goes to `.syncthing.tmp.<name>` first, then `Sync()` → `Rename()` to the final
name on Close (`lib/osutil/atomic.go:19,45-90`; the temp name is generated by `fs.TempName`, skipped
during scanning by `fs.IsTemporary`, and cleaned automatically once it exceeds `TempLifetime` —
`lib/scanner/walk.go:314-320`).

**SyncDash**: [src/apply.rs:140](src/apply.rs:140) calls `std::fs::copy(&src, &dst)` straight into the
final path.

**The concrete bad ending** (not a theoretical risk — large files getting interrupted over SMB is
routine):

> sync mode + archive. `a.psd` on target is being Updated: the old version has already gone to trash
> (recoverable), and the new version is half-written when the network drops → target is left with a
> truncated file whose mtime is new.
> Next compare round: source is unchanged against archive (`s_unchanged=true`), target has changed
> against archive (`t_unchanged=false`)
> → takes the `"target-changed"` branch at [src/compare.rs:352](src/compare.rs:352)
> → **emits `Update source` — writing the truncated file back over source**.
> The good file on the source side is gone (target's old version is still in trash, but nobody would
> think to look there).

**The fix**:
- Add `write_atomic(src, dst, mtime, expect_hash)` to `apply.rs`:
  write `<dir>/.syncdash.tmp.<basename>.<pid>` → (when verifying, check the hash on the temp) →
  `set_mtime` → `fs::rename` over the target.
- Same-volume rename is atomic; cross-volume cannot happen (temp sits in the same directory as dst).
- Add `.syncdash.tmp.*` to `filter.rs`'s built-in excludes so temp files are not scanned into the
  snapshot table.
- When scanning, delete any `.syncdash.tmp.*` older than 24h on the spot (matching syncthing's
  `TempLifetime`).
- `pack`'s staging is already "verify everything before touching target", but its final write to disk
  must go through the atomic write as well.

**Acceptance**: a new test — simulate a half-written failure (inject a failing writer) and assert that
dst is either the old content or absent, never a truncated file.

---

#### P0-2. An unmounted share = a plan that deletes everything on the other side (**catastrophic misjudgment**)

**syncthing**: every folder root must contain the marker directory `.stfolder` (`DefaultMarkerName`,
`lib/config/folderconfiguration.go:37,160-196`); `CheckPath` verifies it exists at `:236`, and if it is
missing the folder is put into an error state and **any sync is refused**. The only reason this rule
exists is to guard against "the mount point isn't mounted".

**SyncDash**: no equivalent check at all. With `target = '\\192.168.0.115\xuanbomiao\...'`, that path
may be an empty directory when the Mac is powered off or SMB drops (or be auto-created locally) →
mirror-mode compare produces "copy everything over" (tens of GB transferred for nothing), and the
reverse job produces "delete everything on target". `run --apply` will do exactly that.

**The fix**:
- Reuse the existing `.ffs-sync` territory-marker concept ([src/territory.rs:11](src/territory.rs:11)),
  adding `syncdash mark <root>` to write `.syncdash-root` (containing the job name + creation time).
- Add `require_marker = true` to the job config (**on by default for new jobs**, off by default for old
  jobs for compatibility, flipped one release later).
- Check at the `scan` stage: root exists + marker exists + root is non-empty (if any of the three fails,
  refuse to emit a table rather than emitting an empty one).
- Extra guardrail (syncthing has none, but our explicit model suits it well): **plan health check** —
  when a single plan's `delete` count exceeds N% (default 50%) of target's total entries, `run` refuses
  `--apply` and requires `--i-know`.
  This is more general than the marker and incidentally catches mis-written filters and mistyped paths.

**Acceptance**: a test — when target points at an empty directory with no marker, mirror compare returns
an error rather than a delete plan.

---

#### P0-3. No disk space preflight

**syncthing**: `CheckAvailableSpace(req uint64)` (`lib/config/folderconfiguration.go:360-375`), with a
default of `MinDiskFree = "1 %"` (`:63`); it checks once before processing files pending pull
(`lib/model/folder_sendrecv.go:495`) and again before the versioner archives (`:1052`).

**SyncDash**: `grep -rn "free_space\|disk" src/` returns nothing. The plan already carries a `size` for
every op — summing them tells you exactly how many bytes will be written — yet it is never checked.
Filling up the target disk looks especially bad over SMB (you fill the other machine's system drive).

**The fix**:
- Depend on `sysinfo` or `fs2` (or write ten lines each for Windows `GetDiskFreeSpaceExW` / unix
  `statvfs` and avoid the dependency).
- Before `apply` starts: `bytes needed = Σ(size of copy/update)`, plus 10% headroom + `min_free`
  (default 1%, configurable per job).
- Not enough → refuse outright and report "need X, Y available, short by Z".
- Trash counts too: Update/Delete move the old file into trash first — a same-volume rename takes no
  space, a cross-volume one does.

---

#### P0-4. Undeletable directories are swallowed silently

**syncthing**: `deleteDirOnDiskHandleChildren` (`lib/model/folder_sendrecv.go:1985-2085`) breaks "cannot
delete" into four cases and **reports each separately**: `errDirHasToBeScanned` (something the db does
not know about → queue a scan), `errDirHasIgnored` (ignored files inside → can never be deleted, the
user must handle it), `errDirNotEmpty` (files the db knows and considers legitimate → inconsistent
state, worth worrying about), plus deleting only the `(?d)`-marked deletable items and temp files.

**SyncDash**: [src/apply.rs:187](src/apply.rs:187) — `Err(_) => Ok(())`, with a comment reading
"non-empty (excluded files still inside, etc.): keep it, not treated as an error". **The behavior is
safe, but the user never learns what happened**: mirror finishes reporting 0 errors, yet that directory
on the other side is still there, the same DeleteDir shows up again on the next compare, and it never
converges.

**The fix**:
- Do not change the deletion policy (keep not recursing — that is correct); change the **reporting**:
  distinguish `NotFound` (really deleted / never there → silent), `DirectoryNotEmpty` (list the first 5
  leftover item names, count as skipped, and print the reason), and other errors (count as errors).
- If every leftover item matches the current job's excludes → say "protected by the filter, this is
  expected behavior; use `--prune-excluded` to clear them along with it".
- Convergence: if the automatic re-compare at the end of `run` finds the same DeleteDir appearing again,
  label it explicitly as "could not be deleted last round".

---

### P1 — high-value capabilities, worth scheduling into v0.6 / v0.7

#### P1-1. Block-level transfer (delta sync) — the biggest performance lever

**syncthing**: files are split into blocks, each hashed separately with SHA-256
(`lib/scanner/blocks.go:42-120`). Block size adapts: starting at 128 KiB and doubling to 16 MiB, aiming
for roughly `DesiredPerFileBlocks` blocks per file (`lib/protocol/bep_fileinfo.go:92-102,402-412`).
On pull it first runs `blockDiff` to work out "which blocks I already have" and transfers only what is
missing (`folder_sendrecv.go:1132-1250`); it also looks for identical blocks in **other folders on the
same machine** and copies them locally (`copyBlockFromFolder:1435`), and picks up leftover blocks from
temp files left by an unfinished transfer (`reuseBlocks:1173-1250`).

**SyncDash**: whole-file copying. Change one sector of a 40 GB VM image → transfer 40 GB.
The README lists the rsync algorithm as a "v2 candidate", but block-level hashing is far simpler than
rsync's rolling checksum, and **we already read the whole file to compute blake3 during scanning —
splitting it into blocks costs almost nothing extra**.

**The fix** (two steps; step one alone captures most of the benefit):

- **Step A (v0.6): the block list goes into the table**
  - `Entry` gains two optional fields: `bs` (block size) and `bh` (`Vec<String>`, the list of block
    hashes). Produced only for files with `size >= 8 MiB`, to keep small files from blowing up the table.
  - Adaptive block size follows syncthing's approach: start at `128 KiB` and double, targeting ≤ 2000
    blocks per file.
  - **Table size problem**: a 40 GB file at 16 MiB blocks = 2560 hashes × 64 hex ≈ 164 KB.
    Countermeasure: the block list **does not go in the main table**; store it in the sidecar
    `<table>.blocks.jsonl` (one line per file's block list), keeping only `bh_ref` (the line number in
    the sidecar) + `blocks_hash` (blake3 of the whole block list, for fast equality checks) in the main
    table. When the ssh pipe scenario does not need the sidecar, `--no-blocks` turns it off.
  - The hash cache (`scan.rs:56-90`) extends along with it: cache lines gain the block list, and if
    `(path,size,mtime)` is unchanged the whole thing is reused.

- **Step B (v0.7): apply / pack put the blocks to use**
  - Local/mounted drives: on `Update`, open dst and compare block by block — skip identical blocks,
    seek+write the differing ones. Combined with P0-1's atomic write: first copy dst to a temp (or use
    `CopyFileRange`/`copy_file_range`/`FSCTL_DUPLICATE_EXTENTS`; syncthing's
    `lib/fs/basicfs_copy_range*.go` has a cross-platform inventory to reference), patch the temp, then
    rename.
  - `pack`: the payload carries only differing blocks plus a block-offset list, so pack size may shrink
    by one to two orders of magnitude. The manifest gains `base_blocks_hash` so the far end can verify
    "my baseline really is the one you think it is" before applying, falling back to the whole file on
    mismatch.
  - Copy syncthing's `blockStats` accounting (total / reused / renamed / pulled) and show "actually
    transferred X MB / logically Y MB" directly in the GUI.

**Acceptance**: change 1 MB in a 40 GB file → less than 32 MB actually transferred; the block-list
sidecar keeps main-table growth under 5%.

---

#### P1-2. Conflict copies: let sync move forward on its own

**syncthing**: a conflict does not stall in place. The loser is renamed to
`name.sync-conflict-20060102-150405-<DEVICEID>.ext` (`folder_sendrecv.go:2219-2222`) and the winner
lands normally. `WinsConflict`'s (`bep_fileinfo.go:212-229`) arbitration order is: not-invalid > newer
mtime > device id from the version vector as tie-break.
`MaxConflicts` caps how many copies are kept per file, deleting the oldest past the cap (`:1888-1898`);
a file that is already a conflict copy does not spawn further conflict copies (`:1863`).

**SyncDash**: conflicts are only reported ([src/compare.rs:356](src/compare.rs:356) and elsewhere), and
the GUI locks the row so it cannot be checked. Safe — but **in daily two-machine use one conflict stalls
that file until a human intervenes**, and humans often do not.

**The fix**:
- Add `on_conflict = "report" | "copy" | "newer"` to the job, with **`report` still the default** (no
  change to existing semantics).
- `"copy"`: compare emits two ops — first `Move` the loser to
  `name.sync-conflict-<YYYYMMDD-HHMMSS>-<host>.ext`, then `Copy` the winner over. Arbitration follows
  syncthing: newer mtime wins, and on a tie the host name breaks it lexicographically (we have no device
  id, so hostname is the stable tie-break).
- Conflict copies themselves need to go on the built-in "excluded from move detection" list (otherwise
  detect_moves will pair them as moves).
- `max_conflicts` (default 5, -1 = unlimited, 0 = off) + cleaning up the oldest.
- GUI: add a "make copy" button on conflict rows, equivalent to enabling it once for that row.

---

#### P1-3. Too many false-positive conflicts — borrow the `PreviousBlocksHash` idea

**syncthing**: concurrent version vectors **≠** a guaranteed conflict. `InConflictWith`
(`bep_fileinfo.go:190-208`) has a neat escape hatch: if the incoming file's `PreviousBlocksHash` equals
my current local `BlocksHash`, then **the other side edited exactly the content I have** — that is not a
concurrent modification, so let it through.
(This is precisely the improvement in [PR#10351](https://github.com/syncthing/syncthing/pull/10351),
cited in the README.)

**SyncDash**: under the archive model, anything that differs from archive on both sides is reported as
`both-changed` ([src/compare.rs:356](src/compare.rs:356)). But one common situation gets caught in the
crossfire: **side A edits, it propagates across, but the archive refresh fails (say, a mid-run Ctrl-C)**
→ next round the two sides actually hold the same content… which `files_equal` already blocks.
The real casualty is: **A edits → propagates to B → B edits again on top of that → archive is still two
generations back** → reported as both-changed, when it is in fact a clean linear history.

**The fix**:
- Upgrade the archive table to **multi-generation**: `archive.jsonl` keeps the entry hash set for the
  last K generations (default 3) (storing only `path → [hash]`, which is tiny).
- Relax the ruling: if target's current hash appears in that path's historical hash set → target is
  merely "sitting on some historical version", not a concurrent edit → one-way propagation instead of a
  conflict.
- This is a **cheap approximation** of version vectors under the archive model; it needs no device id
  and costs almost nothing.
- It does not conflict with P2-1 (true version vectors) — it is the step leading up to it.

---

#### P1-4. mtime precision: read back after writing

**syncthing**: `mtimeFS.Chtimes` (`lib/fs/mtimefs.go:68-80+`) **stats the file straight back** after
setting the time, stores `(ondisk, virtual)` together in the db, and thereafter always reports virtual
to the outside. That way timestamp truncation on FAT (2-second granularity), exFAT, and certain SMB
servers has no effect on equality rulings. Comparison additionally has `ModTimeWindow` as a backstop
(`bep_fileinfo.go:455`).

**SyncDash**: [src/apply.rs:49](src/apply.rs:49) does not read back after `set_mtime`, relying on the
hard-coded `MTIME_SLACK_MS = 2000` tolerance at [src/compare.rs:22](src/compare.rs:22).
At the standard/paranoid levels the hash is a backstop so it hardly matters, but **at
`rigor = "quick"` (no hashing) the tolerance is the only criterion**: a 2-second tolerance can both miss
real changes (a genuine edit within 2 seconds) and produce false ones (SMB offset > 2 seconds).

**The fix**:
- After copying, `set_mtime` → read back with `metadata()` → if they differ, record `(ondisk, intended)`
  in the hash cache file (two extra columns in the existing `hashcache/*.jsonl` will do; no new database
  needed).
- On the next scan, if a file's mtime exactly equals the cached `ondisk` → report `intended` to the
  outside.
- Keep the tolerance as a backstop, but it can narrow to 1s, and let jobs configure `mtime_window` (2s
  for FAT volumes, 0 for NTFS↔APFS).

---

#### P1-5. Add `!` negation to filters

**syncthing**: a single `.stignore` file with prefix modifiers `!` (negation/allowlist), `(?i)`
(case-insensitive), `(?d)` (this item may be deleted and does not block deleting the parent directory),
and `#include` (pull in another file) (`lib/ignore/ignore.go:359-400,500-560`). Rules are evaluated
**top to bottom, first match wins**.

**SyncDash**: [src/filter.rs](src/filter.rs) is two FFS-semantics lists, include and exclude.
FFS compatibility is a genuine advantage (an FFS exclude list can be pasted in verbatim) and should not
be dropped. But a requirement like "exclude `*.log` **but keep** `deploy/important.log`" is awkward to
express with two lists.

**The fix**:
- Support a `!` prefix in the exclude list as an **exception** (a path matching a `!` rule passes
  straight through, ignoring any later exclude). This is a superset of FFS syntax, so pasted FFS rules
  behave exactly as before.
- Borrow `(?d)`'s semantics: add a `deletable = ['*/node_modules/']` category — matched items do not
  participate in sync, but **may be deleted along with** the parent directory (which directly unties the
  "directory can never be deleted" knot from P0-4).
- `#include` is out of scope for now (our config is TOML, so arrays can just be concatenated).

---

### P2 — valuable, but expensive or narrow in payoff

#### P2-1. True N-way: version vectors

**syncthing**: `lib/protocol/vector.go` is 300 lines in total, clean enough to serve as a textbook
implementation:
- `Counter{ID: ShortID, Value: u64}`, held in an array ordered by ID (not a map; comparison is a
  two-pointer linear scan).
- `Update`: `value = max(old+1, unix_now)` — the timestamp guarantees monotonicity even if the counter
  is rolled back.
- `Compare` returns five states: `Equal / Greater / Lesser / ConcurrentGreater / ConcurrentLesser`
  (the last two are not really "concurrent magnitude"; they exist only to give sorting a stable order).

**The cost of porting it to SyncDash** (this is the crux):
- A stable node ID is needed (could be `hostname + a UUID generated on first run`, stored in local
  config).
- The archive has to be upgraded from "last snapshot" to "an index carrying version vectors" — one line
  per path, `(hash, size, mtime, version_vector)`. It can still be JSONL (staying readable and
  auditable), but the semantics change: it is no longer "a snapshot of one scan" but "the global state
  as I know it".
- The vector must be updated after every successful apply, **and "I changed it" must be distinguished
  from "I received it from someone else"** (`Update(me)` for the former, `Merge(peer)` for the latter).
- Pairwise comparison across N nodes degenerates to O(N²); syncthing solves that with P2P gossip, and
  hub-and-spoke is enough for us.

**Conclusion**: this is a v1.0-scale change and **should not be done in v0.6/v0.7**.
The README's judgment (hub-and-spoke is enough for now) is right. When it is actually done,
`vector.go`'s semantics can be rewritten 1:1 in Rust — roughly 200 lines plus a property-test suite
(`Compare`'s reflexivity/symmetry/transitivity properties suit proptest well). The step leading up to it
is P1-3's multi-generation archive.

---

#### P2-2. Trash grows without bound

**syncthing**: three versioners. `trashcan` cleans out expired entries by `cleanoutDays`
(`lib/versioner/trashcan.go:57-100`); `staggered` does **tiered thinning**
(`lib/versioner/staggered.go:47-53,63-110`) — one copy every 30 seconds for the first hour, one per hour
for the rest of the day, one per day within 30 days, and one per week after that, up to `maxAge`;
`simple` keeps a fixed number of copies. After deleting, it also empties the directory
(`empty_dir_tracker.go`).

**SyncDash**: [src/apply.rs:27](src/apply.rs:27) — `trash/<timestamp>/`, **never cleaned**. One new
directory per apply. After a few months of daily runs `%LOCALAPPDATA%\syncdash\trash\` gets substantial,
and finding one file among hundreds of timestamp directories is basically luck.

**The fix**:
- Add `trash_keep_days` (default 30) and `trash_max_bytes` (default 10 GiB) to the job / global config.
- Three subcommands, `syncdash trash list|restore|prune`:
  `list <path-glob>` finds the historical versions of one file across all timestamp directories (this is
  what a trash can is actually for);
  `restore <path> [--at <ts>]`; and `prune` cleans up against the two limits above, emptying directories
  as it goes.
- Call prune opportunistically once at the end of `run` (syncthing drives it from a timer; we have no
  resident process, so it rides on run).
- `staggered`'s thinning algorithm is worth copying semantically — it is far smarter than "keep N days",
  and the implementation is only 40 lines.

---

#### P2-3. Write protection on case-insensitive filesystems

**syncthing**: `caseFilesystem` resolves the real spelling in the directory before writing; if `Foo.txt`
differs from an existing `foo.txt` only by case → it returns `CaseConflictError`
(`lib/fs/casefs.go:27-37`), with a 1-second-TTL LRU directory-name cache to avoid a readdir every time.

**SyncDash**: case folding is done **at compare time** ([src/compare.rs:121](src/compare.rs:121), and
done correctly), but there is no protection **at apply time**: a job with `case_sensitive = true`
running "copy `Foo.txt`" on NTFS silently overwrites the existing `foo.txt`. A self-inflicted corner
case, but genuinely silent data loss.

**The fix**: before apply creates a new file, if the executing side's filesystem is case-insensitive
(detectable — create a temp file in root and stat it with different casing) and the plan contains another
path in the same directory differing only by case → turn it into a Conflict.
Cheap; fold it into P0-2's plan health check.

---

#### P2-4. Metadata changes have no corresponding op

**syncthing**: `shortcutFile` (`folder_sendrecv.go:1253`) — when only permissions/mtime changed, it
transfers no content and changes only the metadata.
`FileInfoComparison` (`bep_fileinfo.go:454-462`) allows ignoring perms / xattr / ownership / blocks as
needed, each dimension toggled independently.

**SyncDash**: [src/table.rs:44](src/table.rs:44) records `mode`, but `files_equal` at
[src/compare.rs:126](src/compare.rs:126) only looks at hash / size / mtime — **`mode` is recorded and
never used**. Add the exec bit to a script on Mac and it still is not there after syncing.
(The `pack` path restores mode, the mounted-drive path does not — inconsistent behavior.)

**The fix**:
- Add `Action::Chmod` (carrying the target mode), emitted only when both sides are unix and
  `sync_mode = true`.
- Add `sync_mode = false` to jobs (off by default, because in the Win↔Mac scenario the Windows side has
  no mode and turning it on would report differences forever).
- It can default to on when both operating systems are unix.
- At the same time, unify mode behavior across the pack and direct paths.

---

#### P2-5. Move pairing for empty / identical-content files is arbitrary

**syncthing**: **the very first thing** `findRename` (`lib/model/folder.go:930-932`) does is
`if len(file.Blocks) == 0 || file.Size == 0 { return false }` — empty files do not participate in rename
attribution.

**SyncDash**: `detect_moves` at [src/compare.rs:183](src/compare.rs:183) buckets by `(hash, size)`.
All empty files have size=0 and the same blake3 → they crowd into one bucket; the same goes for the many
identical `__init__.py`, `LICENSE`, and `.gitkeep` files in a repo.
The pairing result is **still correct content-wise** (convergence is fine), but the `from` field is
picked arbitrarily, so the "rename-detected-by-hash" reason becomes noise — and plan readability is
exactly what SyncDash stands on.

**The fix**: when a bucket has more than one candidate, abandon pairing outright if `size == 0` (falling
back to copy+delete); for non-empty files with multiple candidates the existing three-tier priority
("same parent directory → same file name → arbitrary") is enough — it just has to state the truth in the
reason as `move-detected-by-hash (ambiguous: N candidates)` instead of pretending to be certain.

---

#### P2-6. Scan progress

**syncthing**: `ProgressTicker` + `byteCounter` periodically emit `FolderScanProgress` events carrying
current/total bytes and MiB/s (`lib/scanner/walk.go:55-62,148-200`).

**SyncDash**: the Tauri frontend has three states — "scanning source → scanning target → comparing" —
but no percentage or rate. While scanning a large tree (tens of GB), the user cannot tell whether it is
stuck or running.

**The fix**: add an optional `progress: Option<&dyn Fn(ScanProgress)>` to `scan()`; phase 1 knows the
total byte count once the walk finishes, and phase 2's rayon parallel hashing accumulates hashed bytes
in an `AtomicU64`, calling back every 500 ms. The Tauri side updates the progress bar when it receives
the event — the frontend already has an event channel, so the change is small.

---

### Explicitly **not** copied

| What syncthing has | Why not copy it |
|---|---|
| BEP protocol / TLS / device discovery / relay / NAT traversal | SyncDash deliberately goes through ssh + SMB + tar packs. Not having an entire network stack is a feature, not a defect |
| Resident daemon + web UI + REST API | The Tauri desktop app already exists; a resident process would break the core promise of "dry-run by default, nothing moves until a human clicks" |
| Index database (sequence / LevelDB / SQLite) | JSONL tables are readable, diffable, and pipeable — that is the selling point. The block-list size problem is solved with a sidecar |
| Encrypted folders (`folder_recvenc.go`) | No matching scenario (LAN SMB + ssh) |
| Filesystem watching (inotify / ReadDirectoryChangesW) | See below |
| `Revert` for receive-only folders (`folder_recvonly.go:69`) | Our mirror mode + per-row direction flipping already covers the same need |

**On file watching**: `lib/watchaggregator/aggregator.go`'s aggregation strategy is well worth reading
(debouncing, merging by directory, and degrading to a scan of the whole directory once `maxFiles=512` /
`maxFilesPerDir=128` is exceeded, `:21-25,193-260`).
But introducing a watcher means SyncDash becomes a resident process, which conflicts with the
"explicitly triggered, previewable" positioning.
**Suggested approach**: add an **optional** `syncdash run <job> --watch` in v0.8, where the watcher only
**triggers one compare and pushes the result to the GUI**, never applying automatically.
That gets the "see the diff the moment you finish editing" feel without giving up the
human-confirmation gate.

---

## 3. Release plan

> **Outcome**: all three tiers below (originally v0.6 / v0.7 / v0.8) landed in one go — see the
> implementation status table at the top.
> The original tiering is kept because it records the **reasoning behind the priorities at the time** —
> which is where this plan's value lies.
> v1.0 (true N-way) was **not started**, as planned; only its mathematical prerequisite was delivered.

### v0.6 — safety net (finish all of P0, add no new capability)

- [ ] P0-1 atomic write: temp + fsync + rename; `.syncdash.tmp.*` into the built-in excludes; expired temp files cleaned automatically
- [ ] P0-2 root marker (`.syncdash-root`) + `require_marker` + plan health check (delete-ratio threshold)
- [ ] P0-3 disk space preflight (including the cross-volume trash case)
- [ ] P0-4 classified reporting for DeleteDir failures, no longer silent
- [ ] P2-5 empty files excluded from move pairing; ambiguous pairings annotated truthfully in the reason
- [ ] P2-3 case-conflict preflight at apply time (folded into the plan health check)
- [ ] Regression: the existing 20 compare-matrix tests all green + three new test groups for atomicity/marker/space
- [ ] Real-hardware verification: the full Win → SMB → Mac flow, including recovery behavior after a **deliberately interrupted copy**

**This release adds not one line of new functionality; it is purely about data safety. P0-1 and P0-2
genuinely lose data / waste tens of GB of transfer.**

### v0.7 — block-level transfer + conflict automation

- [ ] P1-1 step A: block-list sidecar + adaptive block size + hash-cache extension
- [ ] P1-1 step B: writing differing blocks in apply (together with the atomic write); `pack` carries only differing blocks + `base_blocks_hash` verification
- [ ] P1-2 conflict copies (`on_conflict` / `max_conflicts`), plus a "make copy" button in the GUI
- [ ] P1-3 multi-generation archive (K=3) to reduce false-positive conflicts
- [ ] P2-6 scan progress events (byte count + rate)
- [ ] Benchmark: change 1 MB in a 40 GB file, actual transfer < 32 MB; main table growth < 5%

### v0.8 — polish and operations

- [ ] P1-4 mtime read-back correction + configurable `mtime_window`
- [ ] P1-5 filter `!` negation + the `deletable` category
- [ ] P2-2 trash retention / size cap + `trash list|restore|prune`
- [ ] P2-4 `Action::Chmod` + unified mode behavior across pack and direct paths
- [ ] Optional `run --watch` (triggers a compare only, never applies automatically)
- [ ] Carried over from the original roadmap: merged display for same-directory renames, GUI job editing, `run --all`, end-to-end ssh

### v1.0 — true N-way (major rework; start only after a separate evaluation)

- [ ] Node ID + version vectors (rewrite `vector.go`'s semantics in Rust + proptest)
- [ ] Upgrade the archive to an index carrying version vectors
- [ ] N-node convergence tests (simulate 3-5 nodes with random edits and a random sync order, assert eventual consistency)

---

## 4. Table schema evolution

Currently `SCHEMA = 1` ([src/table.rs:8](src/table.rs:8)). The changes above touch the table; the plan
is:

```
schema 2 (v0.7)
  Entry  += bs: Option<u32>             block size
         += bh_ref: Option<u64>         line number of the block list in the sidecar
         += blocks_hash: Option<String> blake3 of the whole block list (fast equality check)
  Header += blocks_sidecar: Option<String>  sidecar file name
         += node_id: String             reserved for v1.0; hostname will do for now

  new file <table>.blocks.jsonl -- one line per file: {"path":..,"bs":..,"blocks":[..]}

schema 3 (v1.0)
  archive changes from "snapshot" to "index": each line {path, hash, size, mtime, version: Vector}
```

**Compatibility strategy** (especially important for `pack`/`apply-pack` across versions):
- Read side: accept whenever `schema <= the highest version I support`, letting serde ignore unknown
  fields (the existing structs already follow the `#[serde(default)]` habit; keep it).
- Write side: emit new fields only when both sides' `probe` reports support — `probe` already reports
  schema ([src/main.rs](src/main.rs)'s `Cmd::Probe`), so extending it to report `schema_max` is enough.
- `apply-pack` meeting a schema higher than its own → **refuse explicitly and prompt to upgrade**; do
  not try to guess.

---

## 5. Priorities in one sentence

> **Do not touch any new functionality before v0.6.**
> Atomic writes (P0-1) and mount-point detection (P0-2) can each cause real data loss / tens of GB of
> wasted transfer, and together they are less than a day of work. Block-level transfer is tempting, but
> it is a performance optimization — and optimizing performance on a system that writes truncated files
> back over the source is doing things in the wrong order.
