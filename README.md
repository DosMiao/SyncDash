# SyncDash (`syncdash`)

SyncDash is a safety-first, multi-target file synchronization tool for the command line and desktop. Its core workflow is:

```text
scan roots -> compare snapshot tables -> review a plan -> apply selected operations
```

Snapshots, plans, progress events, and run records are inspectable JSONL artifacts. The same Rust library powers the CLI and the Tauri v2 desktop app, so both shells use the same comparison, safety, transport, and apply logic.

SyncDash supports local folders, mounted shares, SFTP, FTP/FTPS, native SMB, and SSH peers. It detects content-identical moves, maintains sync archives, verifies transfers, preserves replaced data, and treats conflicts or incomplete evidence as review items rather than making destructive guesses.

## What it provides

- Three modes: source-authoritative `mirror`, bidirectional `sync`, and add-only `enrich`.
- One source with multiple selectable targets; each source/target pair keeps its own Compare result and review state.
- JSONL snapshot and plan files suitable for inspection, piping, archiving, and peer execution.
- BLAKE3 evidence, move detection, optional FastCDC delta transfer, write verification, and a durable hash cache.
- Atomic staged writes, root leases, disk-space and deletion-ratio checks, trash, optional versioning, and dry-run defaults.
- FFS-compatible include/exclude masks, Unicode normalization, case handling, Windows-name validation, symlink policies, and Unix-mode preservation.
- A React 19 desktop UI for jobs, Compare, Run Scope, review, Apply, AutoScan, progress, logs, and CSV export.
- Hub-and-spoke synchronization through ordinary local/VFS roots or a `peer://` root whose far-side SyncDash scans and applies locally.

## Architecture

```text
SyncDash/
├── Dev/                        product source
│   ├── src/                    Rust core library and CLI
│   │   ├── base/               foundation, persisted model, and filesystem boundaries
│   │   ├── services/           observability and durable stores
│   │   ├── workflow/           scan/compare/apply pipelines and transfer
│   │   ├── application/        startup, jobs, run history, and orchestration
│   │   └── shell/cli/          command-line contract and dispatch
│   ├── src-tauri/              Tauri desktop crate
│   │   └── src/                app, contracts, features, and grouped IPC commands
│   └── typescript/
│       ├── core/               types, pure domain/application logic, and infrastructure
│       ├── ui/features/        feature-owned React components and hooks
│       ├── ui/pages/           workspace and progress controllers, runtime, and views
│       ├── ui/shared/          focused reusable UI primitive families
│       ├── ui/windows/         thin independent window composition roots
│       └── styles.css          design tokens and all styling
├── Script/                     generators, behavior tests, and architecture audits
├── tests/                      Rust integration tests
├── tools/builder/              cross-platform project Builder adapter
└── dist/                       committed frontend output embedded by Tauri
```

The full placement and dependency rules are in [`Dev/ARCHITECTURE.md`](Dev/ARCHITECTURE.md).
Rust dependencies point downward through the layers; `Dev/src/lib.rs` is the authoritative Rust
contract. Multi-responsibility branches recursively separate models, policy/state, persistence,
coordination, execution, and delivery; architecture audits reject upward dependencies and generic
catch-all directories. `run` selects the transport once. A VFS root such as `sftp://host/path` is
operated by this process, while `peer://host/path` delegates scanning and apply to SyncDash on the
peer.

## Sync behavior

| Mode | New item | Changed item | Deleted item | Move |
|---|---|---|---|---|
| `mirror` | Copy source to target | Source wins | Remove target extra | Replay on target |
| `sync` | Fill either side | Use the last-sync archive; concurrent edits conflict | Propagate only with archive evidence | Replay on the other side |
| `enrich` | Copy source to target | Update only when source is newer | Keep it | No move pairing |

Without a usable archive, `sync` enters a safe mode: it fills missing items, reports differences and possible moves, and avoids attributing a missing file to deletion. A successful sync refreshes its archive atomically. Archive evidence tiers must match the comparison tier.

Content evidence is selected by `rigor`:

| Rigor | Evidence read during a scan | Cache | Verify writes | Typical use |
|---|---|---|---|---|
| `quick` | Metadata only | — | No | Structural review |
| `fast` | Sample changed surfaces | Yes | No | Cloud or media collections |
| `balanced` | Sample changed surfaces | Yes | Yes | Frequent external-drive sync |
| `standard` | Fresh samples for every file | No | Yes | Default daily use |
| `paranoid` | Every byte | No | Yes | Migration or media audit |

Files below 4 MiB are fully read when sampling; larger files use 256 KiB windows at the head, middle, and tail. When sampled digests agree but mtimes diverge, SyncDash escalates to full hashing before deciding equality. `evidence`, cache, and verification fields can override a preset explicitly.

Two timestamps within the run's mtime equality window count as the same instant. The 2-second default is a floor for FAT and SMB rounding, not a fixed tolerance: each run raises the window to the coarser of the two backends' declared mtime precision, so a root that reports whole minutes — an FTP LIST listing — is compared on a minute-wide window. The window a comparison used is recorded with its result, and the desktop's timestamp cues read that number rather than the default.

A directory the scan is refused — a Windows ACL that excludes the running account, a macOS TCC gate on `~/Desktop` or an external volume — does not stop the walk. The scan records the subtree as unread, keeps going, and reports every such path in one pass; the snapshot header carries them, and compare then leaves both sides blind at and under each one. Nothing inside an unread subtree is copied, deleted, or counted as a difference, and the desktop banner, the CLI preflight warning, and the plan header all name the paths so the remedy is per path rather than per run. This matters because a directory is recorded when its parent lists it: without the suppression it would sit in the table with zero children, and under `mirror` zero children is a delete for every file the other side still holds. Excluding an unreadable path in the job's filter also works — the filter is consulted before the path is reported. Only an unreadable *root* still fails the scan, because there is no evidence left to suppress against.

Current snapshot tables use strict schema 2. Headers, evidence kinds, entry shapes, root-relative paths, and content identities are validated exactly; missing or unknown fields are rejected. Move pairing requires a full BLAKE3 identity on each candidate, so a fully hashed small file in a sampled scan can still pair while a sampled digest can never authorize a move. Legacy schema 1 is accepted only at the archive-migration boundary, which retains an immutable backup and migration receipt.

## CLI quick start

Use `syncdash --help` and `syncdash <command> --help` for the complete contract.

```bash
syncdash jobs
syncdash run photos                         # Compare only
syncdash run photos --apply                 # Apply and refresh the archive
syncdash run --all                          # Run every configured job
syncdash gui                                # Open the desktop app

syncdash scan ./source --out source.jsonl
syncdash scan ./target --out target.jsonl
syncdash compare --source source.jsonl --target target.jsonl \
  --mode mirror --out plan.jsonl
syncdash apply plan.jsonl                   # Dry-run
syncdash apply plan.jsonl --apply

syncdash pack plan.jsonl --out package.tar
syncdash apply-pack package.tar             # Validate and dry-run
syncdash apply-pack package.tar --apply
```

Other commands manage territories and generated jobs, credentials and backend capabilities, root markers, logs, trash, version history, restore, peer chunks, and package reception.

## Job configuration

Jobs are TOML files in `%APPDATA%\syncdash\jobs\` on Windows and `~/.config/syncdash/jobs/` on macOS and Linux:

```toml
schema = 4
job_id = "0123456789abcdef0123456789abcdef"
mode = "sync"
source = 'D:\Data\Photos'
targets = ['\\server\backup\Photos', 'sftp://mac/Users/me/Photos']
archive = 'D:\SyncDash\archives\photos.jsonl'

# rigor = "standard"                    # quick | fast | balanced | standard | paranoid
# evidence = "sampled"                  # none | sampled | full
# include = ["*"]
# exclude = ["*/.cache/", "*/*.tmp"]    # FFS-compatible masks
# case_sensitive = false
# require_marker = true
# max_delete_ratio = 0.5
# versioning = true
# autoscan_interval_secs = 30
# autoscan_auto_apply = false
```

`targets` is the only current target authority and contains at least one root. The registry assigns `job_id`; identity survives rename but changes after delete/recreate. Schema v1-v3 jobs migrate on load to canonical targets, AutoScan names, evidence policy, and current filters. Saving writes schema 4, while newer unknown schemas are refused rather than rewritten without fields this build cannot understand.

Each target has an independent Compare/Apply scope. Editing or swapping roots persists the exact selected target and invalidates evidence whose configuration revision no longer matches.

## Roots and peer execution

A root is either a local path/UNC mount or a credential-free phrase:

```text
sftp://user@host/path
ftp://host/path
ftps://host/path
smb://user@host/share/path
peer://host/path|exe=~/bin/syncdash|mount=\\host\share\path
```

Credentials live in the OS credential store and are managed with `syncdash cred`; they do not appear in job phrases, plans, logs, or cache identities. Unknown schemes fail as configuration errors instead of becoming accidental local directories.

For ordinary VFS roots, this process scans and writes through the backend. A peer job probes the far-side build, asks it to scan its local disk, compares locally, sends a verified package for far-side apply, handles any source-side operations through `mount=`, and then refreshes the archive. Both ends use the same build because command and artifact schemas are intentionally strict.

Packages are tar containers with one plan, payload entries, and a versioned manifest. Before extraction or target mutation, `apply-pack` validates structure, schema, operation counts, the exact plan/manifest/payload set, and per-file, plan, and combined BLAKE3 digests. Duplicate, missing, extra, malformed, or digest-incoherent members reject the package.

The supported multi-endpoint topology is hub-and-spoke: create one job and archive per spoke, then use `run --all` or `run --prefix`. This gives predictable pairwise attribution without requiring a resident peer-to-peer database.

## Safety and recovery

- Apply is dry-run by default. Only `--apply` or an authenticated desktop Apply can mutate roots.
- Absolute, drive-prefixed, traversal-shaped, and SyncDash-internal operation paths are rejected before a backend opens.
- Writes stage beside the destination, verify as configured, and publish atomically. Moves claim and verify the exact source before no-replace publication.
- `.syncdash.lock` anchors an immutable generational lease ledger. Ownership is explicit; a missed heartbeat never guesses that another writer is dead.
- Preflight blocks a run before any write when a root is missing, a mount marker is absent, or the volume cannot hold the planned bytes. Delete ratios, the free-space reserve, plan health checks, and a root whose measured filesystem is case-insensitive while the job declares `case_sensitive = true` are reported for an operator to weigh rather than enforced, and capability reports record what the run will do without deciding anything. An unattended AutoScan auto-apply refuses on any of them, blockers and warnings alike.
- Replaced or deleted content goes to local trash or, when enabled, the root's `.version_syncDash/` history. Restore is dry-run by default, validates the complete index/manifest and selected payloads, holds the root lease, and retains displaced current content under `.syncdash/restore/<session>/`.
- Comparison keys normalize Unicode to NFC and fold case by default while I/O preserves each side's original spelling. Windows-illegal names and same-side normalization collisions become explicit plan issues.
- A subtree no scan could read is excluded from the comparison on both sides rather than treated as empty, so a permission problem on one root can never delete the other root's copies. Entries that vanished mid-walk are the separate, genuinely-absent case and are reported as such.

## Desktop app

The desktop app provides job and multi-target editing, folder picking and drag-and-drop, path history and health checks, Compare, a two-sided plan table, per-row direction review, identical-item browsing, CSV export, and logs.

The result workspace separates three concerns:

- **Result Set** selects Differences or the authenticated Identical snapshot.
- **Run Scope** controls execution membership through result type, search, folders, and advanced masks.
- **View** controls grouping, sorting, folding, columns, and path presentation without changing execution.

Every successful desktop Compare receives a random 128-bit `result_id` and is published as an immutable, checksummed artifact in the machine-local Compare-result repository. The index records the exact job identity, configuration revision, target, and latest result for each scope. Switching jobs or targets restores the complete in-session workspace—including review decisions, filters, view, and viewport—and there is no session scope limit that silently discards it. The backend's four-entry hot cache only limits memory; unloading an entry never forgets its disk artifact.

After an application restart, an exact or latest immutable result can be loaded again for inspection, Identical queries, and export. Its execution status is deliberately `application_restarted`, so Apply remains unavailable until a new Compare verifies the filesystem again; review and presentation edits from the previous process are not persisted. Closing a workspace or dismissing an AutoScan candidate does not delete evidence. Permanent deletion occurs only through the explicit forget operation, reached with **Forget Result** in the result bar. It states what it discards and requires confirmation, it is unavailable while a run, an AutoScan verification, a restore, or an open review could still depend on that evidence, and the confirmation is re-answered against current state before anything is discarded. The result leaves the workspace only once the backend has actually forgotten it; a post-commit cleanup failure is reported as a warning rather than pretending the deletion did not commit.

Apply is available only for checked executable rows in a current authenticated Differences result. The backend binds authorization to the job identity, revision, target, plan, capabilities, health report, and exact row decisions, then rechecks them before reserving execution.

Long operations run off the UI thread and use a phase-aware progress window with pause, cooperative stop, rates, ETA, errors, and optional post-run actions. AutoScan belongs to the backend and tracks an exact job identity, revision, target, generation, and ticket. Every trigger still performs Compare; AutoApply requires fresh health, explicit session permission, and a one-use authorization.

Run logs are streamed under the configured log directory:

```text
runs.jsonl
app.jsonl
<run-id>/summary.json
<run-id>/plan.jsonl
<run-id>/run.jsonl
<run-id>/errors.jsonl
<run-id>/items.jsonl
```

The plan records intent; `items.jsonl` records actual per-operation outcomes. Interrupted runs retain the artifacts already flushed and remain marked unfinished.

## Development, testing, and builds

Use the fast behavior suite while developing:

```bash
npm test
npm run typecheck
```

`npm test` covers Rust library/binary tests and executable frontend behavior. Run
`npm run test:integration` for apply, package, restore, lease, or filesystem-transaction changes;
`npm run test:frontend:audit` for TSX/CSP/IPC permission changes; and `npm run test:all` for a
release or broad refactor.

After changing a Rust `#[ts(export_to = ...)]` type, run `npm run gen:types`. After a frontend change, run `npm run build` and include the refreshed committed `dist/`; optimized macOS builds embed it without requiring Node.

Use the repository Builder for builds and launches. Its Windows and macOS launchers share the Rust implementation under `tools/builder/`:

```bash
./builder.command info
./builder.command dev
./builder.command build dist
./builder.command build 123
./builder.command build cli
./builder.command run dist
```

Use the equivalent `builder.bat` commands on Windows. Tiers `1`, `2`, and `3` are Dist, Max, and Release; compact inputs build them sequentially. These optimized tiers package only the desktop artifact: on Windows it stays inside the checkout under `target/builder-tiers/<tier>/` (build/deep cleanup holds it in `tools/builder/.rescue/` across the deletion and restores it afterwards), while macOS publishes it to the durable Builder artifact store. `build cli` is the sole standalone-CLI path, uses the Dist policy, and writes `target/release/syncdash[.exe]`; macOS additionally publishes a durable CLI copy, and desktop tier builds never build the CLI implicitly. `--dry-run --host windows|macos` prints the complete plan without building or launching anything.

Backend behavior is checked by `Dev/src/base/fs/vfs/conformance.rs`; end-to-end mode behavior lives
in `Dev/src/application/run/e2e/`. Live SFTP, SMB, FTP, FTPS, and exFAT lanes require their
documented `SYNCDASH_E2E_*` environment variables and an explicit
`cargo test -- --ignored` invocation; the SMB conformance lanes read `SYNCDASH_SMB_ROOT` and
`SYNCDASH_SMB_URL` instead. `--ignored` also reaches two lanes that take no environment variable:
the macOS FSEvents delivery acceptance test and the OS-credential-store round trip, which touches
the real credential store and is meant to be run explicitly once per machine.
