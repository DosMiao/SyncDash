//! Annotated example of the current persisted job format.

pub const SAMPLE: &str = r#"# <name>.toml in the jobs directory — one file, one job
schema = 4                              # job-file schema; older scalar-target files migrate on load
# job_id is assigned by the registry on first load/save; do not copy it to another job
mode = "mirror"                         # mirror | sync | enrich
source = 'D:\some\dir'                  # a Windows path; on mac/Linux e.g. '/Users/me/Code'
targets = ['\\host\share\dir']          # one or more roots: local paths, smb:// sftp:// ftp:// ftps:// peer://
# archive = '…/syncdash/archive/<name>.jsonl'   # sync mode only; sits beside this jobs/ directory.
#                                       # Without it deletes and moves are not attributed — `syncdash gen-jobs` writes the path for you
# include = ['*']                       # FFS filter-syntax allowlist (empty = everything)
# exclude = ['*/big_temp/', '*/*.log']  # FFS syntax. The ONLY exclude policy besides this tool's own metadata —
#                                       # junk presets (Windows/macOS/Linux/Developer/IDE/Office/sync tools) write
#                                       # their patterns straight into this list, so it always reads as what runs.
#                                       # `syncdash junk` prints the presets; `syncdash scan --junk <ids>` applies them ad hoc
# rigor = "standard"                    # shortcut preset: quick | fast | balanced | standard | paranoid | custom
# --- rigor detail knobs (a value here overrides the preset's axis; the UI writes them all explicitly) ---
# evidence = "sampled"                  # content evidence: none (0 reads) | sampled (256KB each at head/middle/tail) | full (whole file)
# use_cache = false                     # trust the (path,size,mtime) cache? true in fast/balanced; false from standard up = a real read every round
# escalate = true                       # disagreement escalation: digests equal but mtime differs by more than the equality window
#                                       # (at least 2s; coarse-timestamp backends widen it) -> re-verify both sides in full
# verify_writes = true                  # verify after write: hash of the copy stream vs a re-read from disk
# case_sensitive = false                # case-insensitive by default (the NTFS/APFS default)
# symlinks = "exclude"                  # exclude | direct (sync the link itself)
# versioning = true                     # deleted/overwritten files go into each root's .version_syncDash/
#                                       # (browse and recover with syncdash versions / restore; the local trash by default)
#
# --- safety gates ---
# require_marker = true                 # both roots need .syncdash-root before anything is touched
#                                       # (`syncdash mark <root>` writes it; stops an unmounted share from looking like an empty directory)
# min_free_pct = 0.01                   # minimum free ratio to leave after writing; 0 disables
# max_delete_ratio = 0.5                # refuse to run when one side's deletion share exceeds this (--i-know allows it through); 0 disables
# fsync = true                          # fsync the temp file before renaming; turn off if SMB makes it too slow (at your own risk)
#
# --- conflicts and permissions ---
# on_conflict = "report"                # report (default, report only) | copy (the loser is kept as a .sync-conflict copy) | newer
# max_conflicts = 5                     # with on_conflict="copy", how many copies to keep per file (-1 = unlimited)
# sync_mode = false                     # sync unix permission bits (only meaningful when both sides are unix)
#
# --- filter extensions ---
# exclude = ['*/*.log', '!*/audit.log'] # a `!` prefix = exception, beats every other exclude
# deletable = ['*/node_modules/']       # not synced, but may go along when a parent directory is deleted (syncthing's (?d))
#
# --- delta and parallelism ---
# delta = true                          # big files on local/mounted disks written chunk-wise; pays off for SMB uploads, a wash on symmetric links
# parallel = 4                          # Copy/Update parallel width (1 = sequential; over SMB 2-4 streams basically saturate the uplink)
#
# --- AutoScan ---
# autoscan_interval_secs = 30           # maximum full verification interval; local macOS roots also react to FSEvents
# autoscan_auto_apply = false           # run an authorized result automatically; notify/review by default
#
# --- peer targets (optional) ---
# A `peer://` target means the far side runs its own syncdash: it scans its own disk (no hashing
# over a share) and applies a package this side builds. The whole link is in the phrase.
# targets = ['peer://mac/Users/xxx/Code/some/dir|exe=~/Code/SyncDash/target/release/syncdash|mount=\\mac\share\some\dir']
#   exe=    path to syncdash on the far side; omit if it is on PATH
#   mount=  a local path serving the SAME tree. The peer lane only pushes, so the pull (source-side)
#           direction writes through this instead. Omit it and a job that only pushes is unaffected;
#           pull ops are then skipped with a message saying no mount was declared.
"#;
