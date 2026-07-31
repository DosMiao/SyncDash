File-sync tool: **one Rust core library, two shells** — a CLI bin and a Tauri v2 desktop app with a React 19 webview.

## Project Structure

```
SyncDash/
├── src/                    syncdash core library + CLI bin — one directory per layer, dependencies point DOWNWARD
│   ├── foundation/         L0  fmt · time · path · text · names · dirs   (zero in-crate deps)
│   ├── model/              L0  plan · event · table · chunk               (vocabulary only, no I/O)
│   ├── fs/                 L0  staged (atomic write) · lock · ssh · vfs/  (one root = one backend)
│   ├── store/              L1  settings · trash · version · hashcache · mtimefix · migrate
│   ├── obs/                L1  progress · logging · runlog
│   ├── pipeline/           L2  scan/ (local · vfs) · compare/ · apply/ · guard/ · filter
│   ├── transfer/           L2  peer (the ssh lane) · pack
│   ├── job/                L3  the Job schema · territory · junk · rigor
│   ├── run/                L3  orchestrator: local · peer · roots · archive, behind ONE transport router
│   ├── boot.rs             L3  process startup both shells share
│   └── cli/ + main.rs      L4  args (the --help contract) · dispatch
├── src-tauri/              L4  Tauri shell: dto · bridge · state · cmd/{jobs,run,results,edit,logs,shell}
├── typescript/             Vite + React 19 — no UI library, no CSS framework
│   ├── core/               framework-free domain + ALL IPC; components never call invoke() themselves
│   │   └── types/generated/    ts-rs output — never hand-edit
│   ├── ui/                 main window (App.tsx owns session state · components/ · hooks/ · icons.tsx)
│   ├── progress/           the run sub-window (its own entry point, progress.html)
│   └── styles.css          the whole design-token layer — every size and colour in the app resolves here
└── dist/                   frontend build output — committed to git on purpose (the Mac has no node)
```

- `README.md` is the encyclopedia: architecture, mode semantics, the rigor ladder, the archive model, every shipped feature and why. Read it before designing anything; update it in the same change when a contract moves.
- The layering contract actually in force is the `src/lib.rs` header (it also records the two accepted upward edges).
- Plans: `.docs/PLAN-restructure.md` (7-phase Rust restructure, complete), `PLAN-refactor.md`, `PLAN-syncthing-upgrade.md`.
- **`.docs/0 dev note.md` is the user's own file — never edit it.**

Inherited conventions live in the reference project and apply here unchanged:
`D:\Code\Financial\AlexQuant\CLAUDE.md`, `.docs\Skills\comment_policy.md`, `.docs\Skills\phased_commit.md`, `.docs\Skills\git_concurrent_procedures.md`, `Dev\Desktop\NAMING.md`.

## Commands

- Verification after any edit — the whole story, no other tier: `cargo check --workspace`, `cargo test --workspace`, `npm run typecheck`. Done when all three pass with **zero warnings**.
- After changing ANY `#[ts(export)]` type (`src/**` or `src-tauri/src/dto.rs`): `npm run gen:types` (= `node Script/gen-types.mjs`). Without it the frontend silently compiles against a stale shape.
- Any project build, development launch, optimized tier, installer, or launch of an existing artifact must use the repository Builder: `.\builder.bat <suffix>` on Windows or `./builder.command <suffix>` on macOS. Use `info` to discover suffixes; common examples are `dev`, `build dist|max|release|installer`, `build 123`, and `run dist|max|release`, while `build cli` builds only the standalone CLI. Do not assemble an equivalent project build with raw `cargo build` or `npx tauri ...` commands. The verification and type-generation commands documented here remain direct; `npm run build` is allowed only for the separately documented committed-`dist/` refresh, never as a substitute for a desktop Builder command.
- Windows `builder.bat` and macOS `builder.command` are thin launchers for the one Rust adapter under `tools/builder`, using the shared core in the sibling `Experience/builder` repository. Builder tiers `[1]` Dist, `[2]` Max, and `[3]` Release remain pure Cargo on macOS over the committed `dist/` and need no Node; `[D]` Dev and `[A]` Install App do, because Tauri runs `beforeBuildCommand` on both. Compact inputs such as `123` build the selected tiers sequentially.
- The root package is the **CLI**. A bare `cargo build --release` never builds the desktop — `-p syncdash-desktop` is required, and forgetting it leaves a stale GUI binary that looks like a code bug.
- After any frontend change run `npm run build` and commit `dist/` with it: Tauri embeds `dist/` at compile time and the Mac cannot regenerate it.
- `cargo test` does NOT refresh `target/release/syncdash.exe` — rebuild before any binary-level test.
- The user may have the app open: cargo links straight over the exe, so a build dies at the link step with `Access is denied. (os error 5)`. The desktop shell is disposable and may be killed; a running `syncdash.exe` may be mid-`apply`, holding a root heartbeat lock and writing files — report it and ask, never kill it silently.
- Don't launch the GUI to "check" a change unasked. Interactive acceptance is the user's, on their machine, often while they are using it.
- The user tests in their own build and always completes rebuild + restart. Never attribute a persisting bug to staleness — debug the actual code path.

## Git

- Never commit uninvited: `git commit` only when the user asks for one in the current message. Finishing a task or passing verification is NOT authorization — verify, report, leave the worktree dirty. One requested commit doesn't authorize the next.
- When asked: commit directly on `main`, never branch unless asked (solo dev). Don't push unless asked.
- **Other agent sessions run against this same worktree.** Run `git status` + `git diff --stat` before rewriting or restructuring any shared file. Uncommitted work you did not create may be another session still running — stop and ask rather than merging blind. Prefer targeted edits over whole-file writes while anything is in flight, and commit by explicit pathspec (`git commit -o <paths> -m …`), never `git add -A` / `commit -a`.

## Core Working Rules

- User instructions may be in Chinese. Always respond in English. **Everything human-readable in the product is English too** — code, identifiers, comments, `--help` text and user-facing UI strings — spelling **en-US**.
  - Three deliberate survivors that must outlive any sweep: the two CJK **test fixtures** in `src/foundation/text.rs` (`safe_host("主机") == "--"` asserts non-ASCII collapse; an ASCII input would assert nothing), the CJK **font fallbacks** at the end of `--font` in `styles.css` (they render CJK *file paths* from the user's filesystem in the diff table), and the en-GB `cancelled` — it is an identifier throughout *and* a magic string (`user_err` returns `"cancelled"`, the frontend compares against it), so "correcting" it breaks cancel detection in silence.
- Before making changes, read the relevant source and understand the logic. Read **call sites**, never doc comments, when judging whether something is used: `foundation::fmt::pct` and `human_duration` looked like unadopted helpers and were in fact helpers whose semantics fit no call site (a `-1` sentinel the frontend renders as nothing, `100` meaning "nothing to hash").
- Don't be lazy — the user prefers spending more tokens on thinking and understanding.
- **No defensive fallbacks, no partial states.** This is the product's founding principle, not a style preference: a sync tool that guesses destroys data. Never substitute a default for a missing or failed value (`?? d`, `.unwrap_or(default)`, empty `catch`) — fail loudly at the broken site. The reference behaviour is `vfs::spec`: an unknown `xyz://` scheme parses to `UnknownScheme` rather than falling through to "local path", because treating `sfpt://host/x` as relative would create a directory literally named `sfpt:`. Derived surfaces gate on the COMPLETE input set and compute once — show "waiting", never partial or progressive rendering.
- **No compat shims.** Move code and repoint every caller in one change; never leave a `pub use old::path::thing;` forward behind. A forward keeps alive the dependency edge the refactor existed to remove, and leaves two names for one thing. Grep the call sites first to size the move; if it is too big for one change, say so and propose splitting by caller group — don't ship a proxy. One-time data migrations (job-file `schema` bumps) are the exception, and they are real migrations, not shims.
- Code lifecycle: never delete pre-existing functional code without scoped confirmation for that exact item, even at zero callers — report adjacent dead code as a one-line candidate and wait. On code you ARE tasked to change: break APIs freely and update all callers in the same change. Git history is the archive — no `archived/` folders. (`model/vclock.rs` was deleted exactly this way, deliberately, with the reasoning recorded in the README.)
- Warnings get fixed at the root, never suppressed. No file-level `#![allow(dead_code)]` on new or modified files; symbol-level `#[allow(dead_code)]` requires an inline reason.
- Comment policy — default DELETE; audit every comment you touch. Full rule: `D:\Code\Financial\AlexQuant\.docs\Skills\comment_policy.md`. What survives here are the module headers explaining *why* a shape is the way it is; those are load-bearing and are the reason this codebase is navigable.

## Module Rules (Rust)

- **Layering L0→L4, downward only.** Reaching upward is allowed but the module header must say why. Verified mechanically, not asserted: Tarjan over the comment-stripped files reports no SCC larger than one. Two known exceptions, both documented in `src/lib.rs` — the `log_*!` macros are `#[macro_export]` so they carry edges no `use` declares, and counting those the *directory* graph has one accepted cycle (`fs → obs → store → fs`).
- **No barrel files / re-export hubs.** Every `mod.rs` carries real content and callers write the full path (`foundation::fmt::human_bytes`). The crate's only barrel was deleted for laundering `model::table::EntryKind` through `fs::vfs`, hiding that edge at four backends.
- Module shape: a single-file domain stays flat at its parent (`boot.rs`, `pipeline/filter.rs`); only a multi-file domain earns a directory.
- `model` holds **vocabulary**, the engines that produce it live in `pipeline`/`obs`. Keeping the plan format and event schema out of their engines is what makes the file graph acyclic.
- **"Remote" is retired from the library and must not come back.** A **peer** job is one the far side's own syncdash executes (`peer://`, `run::peer`, `transfer::peer`); everything else runs in this process however distant its roots — an `sftp://` root is read and written *here*, down `pipeline::scan::vfs`. `local` vs `vfs` is which primitives exist, not distance. "Remote" survives only in the run log's stored `kind` strings (renaming the producer would split one history in two) and in the user-facing `gen-jobs --remote-host` flag.
- The transport choice is made in exactly one place: `run::{is_peer_job, run_kind, compare, preflight, apply, run_job}`. It used to be written out six times across the two shells, already drifted apart.
- Where a root lives is answered by the **root phrase** (`fs/vfs/spec.rs`) and nowhere else — never by a second job field. Credentials never appear in a phrase; they live in the OS credential store keyed off it.
- New VFS backends implement the sync `Vfs` trait and must pass `fs/vfs/conformance.rs`. `MemVfs` (`fs/vfs/memory.rs`, `#[cfg(test)]`) is what covers the `scan_vfs` lane — `LocalVfs` returns `Some` from `as_local()` and takes a different lane, so a local temp dir exercises nothing there.

## Rust → TypeScript wire types

- Generated with **ts-rs v12**: `#[derive(…, ts_rs::TS)]` + `#[ts(export, export_to = "../typescript/core/types/generated/")]`. The export base is pinned at the workspace root by `TS_RS_EXPORT_DIR` in `.cargo/config.toml` (two crates at different depths would otherwise emit to two places).
- Wire casing is **snake_case**. Do not add `#[serde(rename_all = "camelCase")]` to a SyncDash type — it breaks every IPC call. (The reference project uses camelCase; that is the one convention that does not carry over.)
- `u64`/`i64` need `#[ts(type = "number")]` or ts-rs emits `bigint`, which `JSON.parse` never produces. For `Option<u64>` write `"number | null"` — a bare `"number"` overrides the whole type and silently drops the null.
- Rust doc comments containing `*/` (FFS filter syntax like `*/big_temp/`) terminate the emitted JSDoc block early and produce invalid TypeScript. `Script/gen-types.mjs` escapes them after generation — which is why generation goes through that script and not bare `cargo test`.
- The frontend keeps **no** copy of engine policy. `Job::default()` and the job-file schema are IPC commands (`default_job`, `job_file_schema`) precisely so there is nothing to drift.

## Frontend Rules

- `typescript/core/` is framework-free domain logic plus **every** `invoke` call; components import from `core/`, never from `@tauri-apps/api` directly. A command-name typo is then a compile error, not a rejected promise at click time.
- `@vitejs/plugin-react` stays on **v4** — 5+ requires Vite 8.
- The Vite `manualChunks` name `shared` is load-bearing: without it Rollup names the shared chunk after its representative module, and React + all of `core/` shipped as `assets/zoom.js` with the whole stylesheet as `zoom.css`.
- Anything that mirrors Rust semantics is rebuilt step-for-step and is a standing maintenance obligation: `core/junk.ts::foldExcludeEntry` reproduces `filter::same_exclude_entry` (`text::fold` = NFC + uppercase, then backslash→slash), and `core/format.ts` mirrors `foundation::fmt`. Drift makes the UI disagree with the engine about what the engine is doing.
- **`core/grouping.ts` owns display order, `core/filter.ts` owns membership.** They were once split across the row sort and the group builder, which is the only reason sorting and tree-grouping had to be mutually exclusive.
- `finalIdx` takes `visible`, never `layout.order` — apply order is the engine's order, because a directory delete must follow its children.
- The view is the action set: rows hidden by funnel / search / chips are not applied (FFS semantics), and the confirmation sheet says so.
- No ESLint config exists yet, so `react-hooks/exhaustive-deps` has never run — check hook dependencies by hand.
- The frontend restructure is deliberately unfinished: `App.tsx` (~1000 lines, 84 hooks), `components/` regrouping, the CSS split, the `chmod` tally divergence and the 1:N target-switch stale state are known open items, not discoveries.

## UI Design Rules

- **Zero inline styles.** Tauri injects a nonce for its bootstrap script into the CSP, and per spec a nonce makes browsers ignore `'unsafe-inline'` — so every inline `<style>` block and `style=""` attribute is silently blocked (an all-inline `progress.html` rendered pure white). JS CSSOM writes (`el.style.x = …`) are fine.
- `typescript/styles.css` is the single token layer; its header states the four rules and they are binding. **No literal colour outside the token block, and no literal px font-size outside the `--fs-*` scale** — a rule needing a value the scale lacks means the scale is wrong, not the rule.
- Hard floors: nothing renders below **11px** (base 13px), every text/background pair clears **4.5:1**, and secondary text carrying real content sits near 8:1. Never stack `opacity` on already-dim text — fold the de-emphasis into the token, or it lands near 1.5:1.
- `--text-3` is the **disabled** tier (~3.5:1 light), not a dim tier. Reason columns, counts, sizes and timestamps are data: they stay on `--text-2`.
- Palette and page structure follow GitHub Primer; component vocabulary (`.btn` / `.chip` / `.badge` / `.menu-*` / `.sheet`) is GitDash's. Light-theme hues take a darker step than Primer publishes, because Primer's own success/attention/done measure 4.29–4.48:1 on the hover surface. **A reference palette is a starting point, not a pass** — after any colour change re-run the contrast audit against `--bg`, `--bg-2` **and** `--bg-3`; several hues clear the first and fail the third.
- Measure before acting on an impression. "Looks hazy" turned out to mean the page was twice as bright (canvas luminance 0.0117 → 0.0055), not that the greys were too close; acting on the intuition would have made it worse.
- The five action hues (copy · update · move · delete · conflict) come from one `MARK` map in `typescript/ui/icons.tsx`, serving all three surfaces that speak them (row action, filter chips, toolbar stats) through `.k-*` classes carrying a single `--k` variable. A class that routes to a cell no selector paints is the failure mode to watch: it looks wired and shows nothing.
- The responsive column ladder (`COLS` in `PlanTable.tsx`, keyed on the *scroll container's* width, not the window's) must never drop a sort key with its column — a dropped column hands its key to the surviving column on the same side via `adopts`.
- Width pressure resolves *inside* a row (wrap, ellipsize, scroll), never by widening the app; every container down from `.app` carries the `min-width: 0` that allows it.
- Zoom is the **webview's own** zoom (Ctrl +/−/0), not a CSS font knob, so borders and layout scale with the type and the px arithmetic stays valid.
- A control states what it is; why and how live in `title`.

## Tauri Shell Rules

- `src-tauri/Cargo.toml` MUST keep `[features] default = ["custom-protocol"]`. Without it a bare `cargo build --release` binary loads the Vite devUrl instead of the embedded `dist/`, and the whole window is `ERR_CONNECTION_REFUSED`. `tauri build` passes the feature automatically — this repo builds with bare cargo, so it has to be in `default`.
- **Any command that creates a `WebviewWindow` must be `async fn`.** A sync command runs on the main thread inside IPC and wry needs the event loop pumping to finish creation: the child sticks at `about:blank` and close events queue behind the wedge, making the whole app unclosable. Close children with `destroy()`, not `hide()` — a hidden child keeps the process alive after the main window closes.
- Commands stay thin: validate, call the library, project into a DTO. Anything longer belongs in `syncdash`, where the CLI can reach it too. IPC carries **facts**, never display policy.
- Long work goes through `spawn_blocking` and reports over the `ProgressEvent` stream. There is no `tracing`/`log` crate here on purpose — the event bus is the one diagnostic path, with a file sink on the end. `println!` is a data cable (`scan` writes its table to stdout), not a log.

## Working Style

- Cross-layer consistency: updating all the related code is preferred over a local patch.
- Refactor when it improves correctness, consistency or maintainability. Prefer explicit naming, hierarchical structure and predictable module boundaries; avoid single files being overly long.
- Priority: (1) data safety and correctness → (2) pipeline coherence → (3) stability/performance. This tool deletes and overwrites the user's files: dry-run stays the default, nothing moves without `--apply`, conflicts are never auto-arbitrated, and the trash/versioning path is never bypassed.
- Use relative file paths in responses, e.g. `src/pipeline/compare/mod.rs:120`.

## Doc Maintenance

- When a contract, invariant or workflow changes, update `README.md` and the relevant module header in the same change, and keep cross-links accurate. The README is what the next session reads first.
