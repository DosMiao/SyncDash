# Development source architecture

`Dev/` contains SyncDash product source. The tree follows ownership and dependency direction rather
than language alone: foundational contracts sit at the bottom, workflows depend inward, and each
shell composes those capabilities at the edge.

## Source roots

```text
Dev/
├── src/                    Rust synchronization core and CLI
├── src-tauri/              Tauri desktop crate
└── typescript/             Shared frontend logic and both webview windows
```

Build manifests, repository tooling, and the committed `dist/` bundle stay at the repository root
because they operate on more than one source root. `tests/` is there for a different reason: Cargo
resolves integration tests only at the package root, so its location is a toolchain requirement
rather than an ownership statement. Those three files exercise the library across a crate boundary,
which is what makes them integration tests; everything else lives beside the behavior it protects.

## First-principles structure rules

The tree is organized by reasons to change, not by file size or syntax:

1. **Authority stays with its invariant.** The module that decides whether evidence, a token, or a
   transition is valid also owns the state required to prove that decision.
2. **Pure decisions precede effects.** Models, codecs, validation, and policy are separated from
   persistence, transport, window events, and filesystem mutation.
3. **Orchestrators compose; leaves implement.** A domain façade names the workflow and its public
   surface. Child modules implement one model, policy, repository operation, or execution phase.
4. **Stable formats have explicit owners.** Persisted schemas and wire DTOs do not live inside a UI,
   command handler, or execution loop.
5. **Shells stay thin.** CLI dispatch, Tauri commands, and React window roots translate external
   input and delegate inward; they do not become parallel business-logic layers.
6. **Depth must be earned.** A cohesive single-purpose file stays flat. A directory is introduced
   only when a domain has multiple independently changing responsibilities; generic `utils` and
   catch-all `common` branches are not substitutes for ownership.
7. **Tests follow behavior ownership.** Cross-module invariants live beside the domain façade;
   persistence and execution helpers remain private so tests protect behavior rather than layout.

## Recursive grouping grammar

Every branch applies the same ownership grammar recursively. A domain creates only the groups it
actually needs; empty ceremonial layers are forbidden.

| Group | Owns | May depend on |
| --- | --- | --- |
| `model/` | domain vocabulary, identities, immutable evidence | lower-level contracts only |
| `policy/` | pure validation, classification, and decisions | model |
| `state/` | in-memory state machines and transition invariants | model and policy |
| `repository/` | domain-facing query and mutation coordination | model, policy, state, persistence |
| `persistence/` | codecs, schemas, paths, migrations, and storage I/O | model and lower-level adapters |
| `controller/` or `use_cases/` | one user/workflow intent across domain collaborators | the groups above |
| `runtime/` or `execution/` | long-lived workers and effectful execution phases | controller inputs and adapters |
| `components/` | rendering and interaction surfaces | presentation models and controllers |
| `hooks/` | React ownership and reusable UI orchestration | application state and presentation models |

The inward direction inside a feature is normally `delivery -> controller -> repository/state ->
policy -> model`; persistence and platform adapters sit beside the state they serve, never inside a
command handler or component. Names such as `utils`, `helpers`, `common`, and `misc` are rejected
when a more specific owner exists.

A file earns a child directory when it changes for two or more independent reasons, combines pure
decisions with effects, owns multiple mutable authorities, or implements multiple separately testable
use cases. Size is diagnostic, not the rule: one cohesive state machine may remain large, while a
short file that mixes authorization and persistence should split. `mod.rs`, page roots, and window
roots are façades: they document the domain, declare children, expose the intended surface, and
perform only the minimal composition that belongs to the whole branch.

## Rust core and CLI

```text
Dev/src/
├── base/
│   ├── foundation/         paths, time, formatting, naming, and platform primitives
│   ├── model/              persisted plan, event, table, and chunk vocabulary
│   └── fs/                 filesystem boundaries, VFS backends, staging, locks, and watches
├── services/
│   ├── obs/                logging and live progress
│   └── store/
│       ├── scan_state/     bound format, location, reporting, and rebuild policy
│       └── version/        retained versions and transactional restore
├── workflow/
│   ├── pipeline/
│   │   ├── scan/local/     model, state, progress, discovery, and stable hashing
│   │   ├── compare/        matching, evidence, planning policy, and conflicts
│   │   └── apply/          validation, lease, reporting, coordination, and execution
│   └── transfer/           peer transport and verified packages
├── application/
│   ├── boot.rs             process initialization shared by both shells
│   ├── job/
│   │   ├── model.rs        persisted job vocabulary
│   │   ├── policy/         validation and runtime projection
│   │   └── persistence/    codec, migrations, registry, and atomic mutation
│   └── run/
│       ├── archive.rs      the sync-mode record of what the two sides last agreed on
│       ├── e2e/            cross-lane pipeline smoke and safety checks (test-only)
│       ├── history/        model, codec, migration, recording, queries, and retention
│       ├── local/          Compare, guarded Apply, and execution-loop orchestration
│       ├── peer/           configuration, probe, package, Compare, and guarded Apply
│       ├── roots.rs        root-phrase resolution shared by both lanes
│       └── watch/          watch validation, trigger state machine, and behavior tests
├── shell/cli/
│   └── commands/           jobs, snapshots, packages, recovery, history, and system delivery
├── lib.rs                  stable public module map and dependency contract
└── main.rs                 CLI composition root
```

Dependencies point from `shell -> application -> workflow -> services -> base`. A layer may depend
on its own layer or anything below it, never above it. `lib.rs` maps the physical tree to the stable
public modules (`syncdash::fs`, `syncdash::pipeline`, and so on), so directory ownership does not
force callers through compatibility re-export hubs. Durable run history belongs to
`run::history`, not observability, because it binds run records to jobs and orchestration.
Within the workflow layer, `pipeline/compare/` separates matching from planning policy and evidence,
while `transfer/pack/` separates its format, deterministic creation, staging, and application.
`pipeline/scan/local/` owns traversal and hashing as sibling capabilities, and
`pipeline/apply/` keeps plan validation and lease/reporting policy outside the mutation executor.
Application-level peer orchestration recursively separates connection configuration, probing,
package lifetime, Compare, Apply policy, and execution. The CLI façade dispatches into
capability-owned command groups rather than implementing every command in one match.

## Tauri desktop backend

```text
Dev/src-tauri/src/
├── app/                    state construction, window lifecycle, and command registration
├── contracts/              Rust-to-TypeScript wire DTOs grouped by feature
├── features/
│   ├── autoscan/           model, authoritative state, controller, runtime, and worker
│   ├── compare/
│   │   ├── export/          filename, row presentation, rendering, and receipts
│   │   └── evidence/
│   │       ├── model/       errors, scope identity, result, execution, and verification data
│   │       ├── persistence/ strict codecs, index integrity, paths, and disk I/O
│   │       ├── repository/  publication, exact lookup, query, mutation, and verification
│   │       └── state/       cache, registry, expiry, workspace, and execution status
│   ├── operations/
│   │   ├── apply/          preparation, review, and authorized execution
│   │   ├── authorization/  challenges, target policy, Compare/Apply review, and token store
│   │   ├── autoscan_authority.rs  AutoScan permit and verification-ticket vocabulary
│   │   ├── compare/        review, approval, and authorized execution
│   │   ├── decisions.rs    shared row-authentication rules for reviewed operations
│   │   ├── events/         run-event model, repository, sink, and throttle
│   │   ├── execution/      shared execution guards and error classification
│   │   ├── lifecycle/      coordinator, leases, preparation, reservation, and control
│   │   ├── projection.rs   operation state projected for delivery
│   │   └── target/         registered target resolution and revision validation
│   ├── jobs/               target resolution and mutation effects
│   └── settings/           authorization, transactional save, and log selection
├── ipc/
│   ├── commands/
│   │   ├── autoscan.rs     AutoScan arm, disarm, and status delivery
│   │   ├── compare/        workspace, export, identical results, and reveal delivery
│   │   ├── desktop/        progress-window construction and power actions
│   │   ├── job_editor.rs   job-editor endpoint readiness delivery
│   │   ├── jobs/           query, projection, mutation, and event delivery
│   │   ├── logs/           run-history query and artifact reveal delivery
│   │   ├── operations/     thin Apply/Compare role checks and use-case delegation
│   │   └── settings/       query, save, and selection delivery
│   └── native/             dialogs and native adapters shared by commands
├── secure_random.rs        secure opaque identifiers for desktop authorities and evidence
├── window.rs               stable window identities shared across delivery boundaries
└── main.rs                 desktop entry point only
```

Contracts, window identities, and shared primitives are leaves. Features own state and safety
invariants. The IPC root owns window-role authorization but imports the same lower-level identities
as feature event delivery; features never depend upward on IPC. Commands validate the caller and
delegate to features rather than becoming a second business-logic layer. `app` is the only
composition root and the only place that registers the complete command surface.

## TypeScript and React

```text
Dev/typescript/
├── core/
│   ├── types/generated/    Rust-owned snake_case wire contracts
│   ├── domain/             pure compare, job, path, and run logic
│   ├── application/        pure reducers, authorities, review state, and use-case policy
│   ├── infrastructure/     window-scoped Tauri adapters and durable browser preferences
│   └── shared/             framework-free formatting helpers
├── ui/
│   ├── features/           feature-owned components, controllers, hooks, and models
│   ├── pages/
│   │   ├── workspace/
│   │   │   ├── components/ page-owned toolbar, sidebar, status bar, and results section
│   │   │   ├── controller/ route composition and typed runtime-to-view adapters
│   │   │   ├── model/      page vocabulary and immutable presentation inputs
│   │   │   ├── presentation/ complete page views with no platform effects
│   │   │   ├── view-model/ derived Compare result presentation
│   │   │   └── runtime/    use-case groups: execution, state authorities, and platform effects
│   │   └── progress/       controller over launch, controls, power, events, and window runtime
│   ├── shared/
│   │   ├── components/     focused feedback, floating, menu, and overlay primitive families
│   │   ├── errors/         window-level error boundary and reporting surface
│   │   ├── hooks/          reusable scroll, zoom, and status bindings
│   │   ├── icons/          shared icon set
│   │   ├── interaction/    the interaction-layer stack and command resolution
│   │   └── status/         the status authority and its React binding
│   └── windows/
│       ├── main/           thin workspace-window composition root
│       └── progress/       thin independent progress-window composition root
└── styles.css              centralized tokens and CSP-safe styling
```

Dependencies point from `ui -> infrastructure/application -> domain -> generated types`. Domain
and application code do not import React, Tauri, or browser persistence. All `@tauri-apps` imports
and literal IPC invocations live in `core/infrastructure/tauri`. Shared UI cannot depend on a
feature, page, or window. Window roots depend on exactly one page; the progress page cannot reach
workspace features or authority.
Feature and page `model/` branches cannot reach platform delivery; `components/` cannot reach
runtime effects; page controllers cannot bypass runtime to import infrastructure; page façades
import exactly one controller. Generic file or directory names such as `support`, `helpers`, and
`utils` are rejected by the source-tree audit because every branch must name the responsibility it
owns; empty ceremonial branches are rejected as well.

## Placement guide

| New responsibility | Owner |
| --- | --- |
| Persisted engine vocabulary or codec | `Dev/src/base/model/` |
| Filesystem/VFS capability | `Dev/src/base/fs/` |
| Settings, cache, trash, or version storage | `Dev/src/services/store/` |
| Scan/compare/apply behavior | `Dev/src/workflow/pipeline/` |
| Job or run orchestration | `Dev/src/application/` |
| CLI-only behavior | `Dev/src/shell/cli/` |
| Desktop state or safety invariant | `Dev/src-tauri/src/features/` |
| Desktop wire type | `Dev/src-tauri/src/contracts/` |
| Tauri command exposure | `Dev/src-tauri/src/ipc/commands/` |
| Pure frontend policy | `Dev/typescript/core/domain/` or `application/` |
| Tauri/local-storage integration | `Dev/typescript/core/infrastructure/` |
| Reusable feature presentation or controller | the owning `Dev/typescript/ui/features/` branch |
| Workspace or progress page orchestration | the owning `Dev/typescript/ui/pages/` branch |
| Window bootstrap only | `Dev/typescript/ui/windows/` |
