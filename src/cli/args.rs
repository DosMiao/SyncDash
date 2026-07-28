//! The command-line surface: every flag and subcommand, and nothing that acts on them.
//!
//! Kept apart from the dispatch deliberately. This is the contract a user reads in `--help` and
//! scripts against, so a change here is a change to a public interface; the code that carries it
//! out is next door in `mod.rs` and can be reorganized freely without touching this.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(name = "syncdash", version, about = "Table-driven multi-node file sync (scan -> compare -> apply)")]
pub struct Cli {
    /// With no subcommand (e.g. double-clicking the exe), open the GUI directly
    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

#[derive(Clone, ValueEnum)]
pub enum Mode {
    Mirror,
    Sync,
    Enrich,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Print this machine's environment info (used for remote probing: this is what runs over ssh on the far side)
    Probe,
    /// List job configs (%APPDATA%\syncdash\jobs\*.toml)
    Jobs,
    /// Run a job: scan both sides → compare → (with --apply) execute + refresh the archive. A job whose target is a `peer://` root takes the ssh peer pipeline
    Run {
        /// Job name (a filename in the jobs directory) or a toml path; omit it and use --all / --prefix
        job: Option<String>,
        /// Run every job
        #[arg(long)]
        all: bool,
        /// Run only jobs whose name starts with this (e.g. cs-)
        #[arg(long)]
        prefix: Option<String>,
        #[arg(long)]
        apply: bool,
        /// Allow the "plan health check" through (deletion share too high). A missing marker / insufficient space still blocks
        #[arg(long = "i-know")]
        i_know: bool,
        /// Consent to the capability degradations a remote backend forces (each one is listed first).
        /// Separate from --i-know on purpose: two different risks, two separate nods
        #[arg(long = "accept-caps")]
        accept_caps: bool,
        #[arg(short, long)]
        verbose: bool,
        /// M6 watch: loop compare → (on differences, in auto mode) apply → sleep. Ctrl-C to stop
        #[arg(long)]
        watch: bool,
        /// Watch interval in seconds (defaults to the job's watch_interval_secs, then 30)
        #[arg(long)]
        interval: Option<u64>,
        /// Apply automatically when watch finds differences (same as watch_auto_apply = true in the job)
        #[arg(long = "auto-apply")]
        auto_apply: bool,
    },
    /// Open the graphical interface (the Tauri desktop app; the old egui UI was retired in v0.9)
    Gui,
    /// Print the junk exclude presets and the exact patterns each one contributes to a job's `exclude`
    Junk {
        /// Emit just the patterns of the given presets, one per line — pasteable straight into a job's exclude
        #[arg(long, value_delimiter = ',')]
        patterns: Option<Vec<String>>,
    },
    /// Scan a directory and produce a snapshot table (JSONL, stdout by default — friendly to ssh pipes)
    Scan {
        root: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
        /// Skip content hashing (fast, but comparison degrades to size+mtime and move detection becomes impossible)
        #[arg(long)]
        no_hash: bool,
        /// Rigor preset: quick (0 reads) | fast (sampling + cache) | standard (really samples every file each round, the default)
        /// | paranoid (reads every byte each round)
        #[arg(long, default_value = "standard")]
        rigor: String,
        /// Detail override: content evidence none | sampled | full (overrides the preset)
        #[arg(long)]
        evidence: Option<String>,
        /// Detail override: hash cache on | off (overrides the preset)
        #[arg(long)]
        cache: Option<String>,
        /// [legacy flag] Ignore the cache and re-read everything
        #[arg(long)]
        force_rehash: bool,
        /// [legacy flag] Sampling + cache (≈ --rigor fast)
        #[arg(long)]
        fast: bool,
        /// Record the symlink itself (its target string); symlinks are ignored by default
        #[arg(long)]
        symlinks_direct: bool,
        /// Junk presets to apply, comma-separated (`syncdash junk` lists them). `none` applies no preset —
        /// what a job passes, since a job's own `exclude` already spells its junk rules out in full
        #[arg(long, default_value = "windows,macos", value_delimiter = ',')]
        junk: Vec<String>,
        /// Extra excludes (FFS filter syntax, e.g. */big_temp/ or */*.log; repeatable)
        #[arg(long)]
        exclude: Vec<String>,
        /// Print hashing progress to stderr (percentage + MiB/s)
        #[arg(long)]
        progress: bool,
    },
    /// Compare two snapshot tables and produce an action plan (JSONL)
    Compare {
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        target: PathBuf,
        #[arg(long, value_enum, default_value = "mirror")]
        mode: Mode,
        /// The last sync's archive (what sync mode attributes adds and deletes against; the Unison approach)
        #[arg(long)]
        archive: Option<PathBuf>,
        /// With no archive in sync mode, resolve differences by "newer wins" (by default it only reports conflicts)
        #[arg(long)]
        resolve_newer: bool,
        /// Case-sensitive matching (insensitive by default — the NTFS/APFS behavior)
        #[arg(long)]
        case_sensitive: bool,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// List the sync territories marked by .ffs-sync (the CodeSync ecosystem)
    Territories {
        root: PathBuf,
    },
    /// Generate a cs-<slug>.toml job for every .ffs-sync-marked territory (syncdash's take on the CodeSync generator)
    GenJobs {
        root: PathBuf,
        #[arg(long)]
        target_root: PathBuf,
        #[arg(long, default_value = "sync")]
        mode: String,
        #[arg(long, default_value = "standard")]
        rigor: String,
        /// Generate remote-pipeline jobs: the ssh host alias (e.g. mac)
        #[arg(long)]
        remote_host: Option<String>,
        /// Remote root path prefix (the remote's own local path, e.g. /Users/xxx/Code)
        #[arg(long)]
        remote_root_base: Option<String>,
        /// Path to the remote syncdash (defaults to assuming it is on PATH)
        #[arg(long)]
        remote_exe: Option<String>,
        /// Junk presets to seed each job's `exclude` with, comma-separated (`syncdash junk` lists them;
        /// `none` seeds nothing). The patterns are written into the job file in full — nothing is applied
        /// on top of what the file says. Defaults to windows,macos,dev: a .ffs-sync marker means a git-kept
        /// code tree, and two-way syncing .git corrupts the repository
        #[arg(long, default_value = "windows,macos,dev", value_delimiter = ',')]
        junk: Vec<String>,
        /// Overwrite existing cs-*.toml jobs, discarding edits made to them. Off by default: a generated
        /// job belongs to whoever edited it, and silently restoring a filter they deleted can stop data
        /// being backed up without anyone being told
        #[arg(long)]
        force: bool,
    },
    /// Receive a file on stdin and write it to path (used to ship remote packages: this runs over ssh on the far side, binary-safe on both platforms)
    Recv {
        path: PathBuf,
    },
    /// Emit the FastCDC chunk table for the given files (used for delta transfer; one JSON line per file)
    Chunks {
        #[arg(long)]
        root: PathBuf,
        /// Relative path; repeatable
        #[arg(long = "file")]
        files: Vec<String>,
    },
    /// Pack the plan's target-side operations into a tar (payload + plan + a two-hash manifest) for the far end's apply-pack
    Pack {
        plan: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// Override the source root in the plan header
        #[arg(long)]
        source_root: Option<PathBuf>,
    },
    /// Execute a package on the target machine: verify the plan hash → verify each file's hash → execute (lock + trash + verify after copy). dry-run by default
    ApplyPack {
        pkg: PathBuf,
        /// Override the target root in the plan header
        #[arg(long)]
        target_root: Option<PathBuf>,
        #[arg(long)]
        apply: bool,
        /// Delete the package file on success (the remote pipeline's cleanup step, free of shell-dialect differences)
        #[arg(long)]
        remove_pkg: bool,
        /// Put deleted/overwritten files in target's .version_syncDash/ (instead of the local trash)
        #[arg(long)]
        versioning: bool,
        #[arg(short, long)]
        verbose: bool,
    },
    /// List a root's version history (.version_syncDash); --prune N keeps only the newest N
    Versions {
        root: PathBuf,
        #[arg(long)]
        prune: Option<usize>,
    },
    /// Recover files from the version history (dry-run by default; --file is repeatable, omit it for the whole version)
    Restore {
        root: PathBuf,
        #[arg(long)]
        version: String,
        #[arg(long = "file")]
        files: Vec<String>,
        #[arg(long)]
        apply: bool,
    },
    /// Write the `.syncdash-root` mount-point marker in root (pairs with the job's require_marker to guard against an unmounted share)
    Mark {
        root: PathBuf,
        /// Job name recorded in the marker file (for humans only)
        #[arg(long, default_value = "")]
        job: String,
        #[arg(long, default_value = "")]
        note: String,
    },
    /// Run history (M4): an overview of every apply's outcome; --prune-days N prunes old records
    History {
        /// Only this job (omit for all)
        job: Option<String>,
        /// Maximum number of rows to show
        #[arg(long, default_value_t = 30)]
        limit: usize,
        /// Prune records and detail files older than N days
        #[arg(long)]
        prune_days: Option<u64>,
    },
    /// Central logs (v0.10): list runs / view one run's three manifests / prune / show where the directory is
    Logs {
        #[command(subcommand)]
        cmd: LogsCmd,
    },
    /// The local trash: view / recover / prune
    Trash {
        #[command(subcommand)]
        cmd: TrashCmd,
    },
    /// Credentials for remote roots — kept in the OS store (Windows Credential Manager / macOS Keychain), never in a job file
    Cred {
        #[command(subcommand)]
        cmd: CredCmd,
    },
    /// Connect to a root and print its full capability sheet (what preflight will reason from)
    Caps {
        /// A root phrase: a local path, smb://…, sftp://…, ftp://…
        phrase: String,
    },
    /// Execute a plan. dry-run by default; only --apply touches anything
    Apply {
        plan: PathBuf,
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        source_root: Option<PathBuf>,
        #[arg(long)]
        target_root: Option<PathBuf>,
        #[arg(long)]
        trash: Option<PathBuf>,
        /// Re-read and verify blake3 after copying (paranoid)
        #[arg(long)]
        verify: bool,
        /// Put deleted/overwritten files in each root's .version_syncDash/ (instead of the local trash)
        #[arg(long)]
        versioning: bool,
        /// Write large files delta-wise in FastCDC chunks (one extra read of the target buys a lot fewer written bytes; pays off for SMB uploads)
        #[arg(long)]
        delta: bool,
        /// Do not fsync the temp file before renaming (fast, but a power cut may lose the last writes)
        #[arg(long)]
        no_fsync: bool,
        #[arg(short, long)]
        verbose: bool,
    },
}

#[derive(Subcommand)]
pub enum CredCmd {
    /// Store the password for a remote phrase (prompts without echo; secrets never touch argv or job files)
    Set {
        /// The remote phrase, e.g. "smb://ben@server/share" — server+user derive the entry, the path plays no part
        phrase: String,
        /// Read the password from stdin instead of prompting (for scripts; beware shell history when piping)
        #[arg(long)]
        stdin: bool,
    },
    /// Remove the stored password for a phrase
    Rm { phrase: String },
    /// List stored credential accounts (names only — secrets stay in the OS store)
    Ls,
    /// Connect once and report what happened, step by step
    Test { phrase: String },
}

#[derive(Subcommand)]
pub enum LogsCmd {
    /// List runs (newest → oldest). **Including interrupted runs** — the ones missing from the index, with only a directory left
    List {
        /// Only this job (omit for all)
        job: Option<String>,
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
    /// View one run's artifacts. Shows the event stream by default
    Show {
        /// Run id (= the directory name, the column in `syncdash logs list`)
        run_id: String,
        /// The error manifest
        #[arg(long)]
        errors: bool,
        /// The execution manifest (one line per op, its actual outcome)
        #[arg(long)]
        items: bool,
        /// The plan manifest (what this run **intended** to do — compare it with the execution manifest to see what never got its turn)
        #[arg(long)]
        plan: bool,
        #[arg(long, default_value_t = 5000)]
        limit: usize,
    },
    /// Prune by the retention policy (defaults come from keep_days / max_total_mb in settings)
    Prune {
        #[arg(long)]
        keep_days: Option<u64>,
        #[arg(long)]
        max_total_mb: Option<u64>,
    },
    /// Print the log directory's location
    Dir,
}

#[derive(Subcommand)]
pub enum TrashCmd {
    /// List every trash batch (time, file count, size)
    Runs,
    /// Search all batches for a path's historical versions (substring match, newest first)
    Find { pattern: String },
    /// Recover files into the given root. dry-run by default
    Restore {
        pattern: String,
        #[arg(long)]
        into: PathBuf,
        /// A specific batch id (defaults to the newest version)
        #[arg(long)]
        run: Option<String>,
        #[arg(long)]
        apply: bool,
    },
    /// Prune by the retention policy. dry-run by default
    Prune {
        /// Delete every batch older than this many days (0 = off)
        #[arg(long, default_value = "30")]
        keep_days: i64,
        /// Total size cap in GiB (0 = off)
        #[arg(long, default_value = "10")]
        max_gib: u64,
        /// Turn off staggered thinning (dense recently, sparse further back)
        #[arg(long)]
        no_staggered: bool,
        #[arg(long)]
        apply: bool,
    },
}
