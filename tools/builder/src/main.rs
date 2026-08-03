use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use builder_core::{
    confirm_cleanup, execute_cleanup, format_duration, load_cleanup_manifest,
    normalize_standard_command, parse_common_args, parse_tier_selection, plan_cleanup,
    print_cleanup_plan, print_standard_info, project_root_from_environment,
    standard_project_command, verify_cleanup_plan_authorization, ArtifactBundleEntry,
    ArtifactSourceKind, BuildError, BuildResult, BuildTier as Tier, CleanLevel, CleanPlan,
    CleanTargetSelection, CleanupManifest, Host, InfoLine, Runtime,
};

const PROJECT_ID: &str = "syncdash";
const PROJECT_NAME: &str = "SyncDash";
const VITE_PORT: u16 = 5173;
const BUNDLE_ID: &str = "com.dosmiao.syncdash";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!();
            eprintln!("  SYNCDASH BUILDER FAILED: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> BuildResult<()> {
    let args = parse_common_args()?;
    let root = project_root_from_environment(PROJECT_ID)?;
    let runtime = Runtime::new(root, &args)?;
    let command = if args.command.is_empty() {
        interactive_command(&runtime)?
    } else {
        normalize_command(&args.command)
    };
    let _operation_lock = if command_requires_operation_lock(&command) {
        Some(runtime.acquire_operation_lock()?)
    } else {
        None
    };

    match command.as_slice() {
        [action] if action == "quit" => Ok(()),
        [action] if action == "dev" => dev(&runtime),
        [action, target] if action == "build" && target == "cli" => build_cli(&runtime),
        [action, target] if action == "build" && target == "installer" => build_installer(&runtime),
        [action, selection] if action == "build" => {
            build_tiers(&runtime, &parse_tier_selection(selection)?)
        }
        [action, tier] if action == "run" => run_tier(&runtime, tier),
        [action] if action == "kill" => kill_only(&runtime),
        [action] if action == "unlock" => kill_and_unlock(&runtime),
        [action] if action == "clean" => clean(&runtime, CleanLevel::Build),
        [action, level] if action == "clean" => clean(&runtime, CleanLevel::parse(level)?),
        [action] if action == "app-self" => build_app_self(&runtime),
        [action] if action == "reveal" => reveal(&runtime),
        [action] if action == "doctor" => doctor(&runtime),
        [action] if action == "info" => info(&runtime),
        [action] if action == "help" => {
            print_help();
            Ok(())
        }
        _ => Err(BuildError::new(format!(
            "unknown SyncDash builder command: {}",
            command.join(" ")
        ))),
    }
}

fn interactive_command(runtime: &Runtime) -> BuildResult<Vec<String>> {
    standard_project_command(runtime, "SyncDash Builder")
}

fn normalize_command(input: &[String]) -> Vec<String> {
    let command = normalize_standard_command(input);
    match command.as_slice() {
        [action, target] if action == "build" && target == "desktop" => {
            vec!["build".to_owned(), "dist".to_owned()]
        }
        [action, target] if action == "run" && target == "desktop" => {
            vec!["run".to_owned(), "dist".to_owned()]
        }
        _ => command,
    }
}

fn command_requires_operation_lock(command: &[String]) -> bool {
    command.first().is_some_and(|action| {
        matches!(
            action.as_str(),
            "dev" | "build" | "run" | "kill" | "unlock" | "clean" | "app-self"
        )
    })
}

fn cleanup_manifest(runtime: &Runtime) -> BuildResult<CleanupManifest> {
    let manifest = load_cleanup_manifest(runtime.root())?;
    if manifest.project() != PROJECT_ID {
        return Err(BuildError::new(format!(
            "cleanup manifest project {:?} does not match {PROJECT_ID:?}",
            manifest.project()
        )));
    }
    Ok(manifest)
}

fn release_dir(runtime: &Runtime) -> PathBuf {
    runtime.path("target/release")
}

fn desktop_binary(runtime: &Runtime) -> PathBuf {
    release_dir(runtime).join(format!("syncdash-desktop{}", runtime.host().exe_suffix()))
}

fn cli_binary(runtime: &Runtime) -> PathBuf {
    release_dir(runtime).join(format!("syncdash{}", runtime.host().exe_suffix()))
}

fn cargo_environment(
    runtime: &Runtime,
    base: &[(String, Option<String>)],
) -> Vec<(String, Option<String>)> {
    let mut environment = base
        .iter()
        .filter(|(name, _)| name != "CARGO_TARGET_DIR")
        .cloned()
        .collect::<Vec<_>>();
    environment.push((
        "CARGO_TARGET_DIR".to_owned(),
        Some(runtime.path("target").display().to_string()),
    ));
    environment
}

fn tier_root(runtime: &Runtime) -> BuildResult<PathBuf> {
    runtime.artifact_path(PROJECT_ID, "tiers")
}

fn tier_dir(runtime: &Runtime, tier: Tier) -> BuildResult<PathBuf> {
    Ok(tier_root(runtime)?.join(tier.key()))
}

fn tier_desktop_binary(runtime: &Runtime, tier: Tier) -> BuildResult<PathBuf> {
    Ok(tier_dir(runtime, tier)?.join(format!(
        "SyncDash-{}{}",
        tier.output_label(),
        runtime.host().exe_suffix()
    )))
}

fn free_desktop(runtime: &Runtime) -> BuildResult<()> {
    println!();
    println!("  [Kill] Freeing syncdash-desktop ...");
    let needles = [
        format!("{}/", runtime.path("target").display()),
        format!("{}/", runtime.path("node_modules").display()),
        format!("{}/", runtime.project_artifact_root(PROJECT_ID)?.display()),
    ];
    if runtime.dry_run() {
        runtime.print_dry_run("stop this checkout's SyncDash desktop processes");
        return Ok(());
    }
    let processes = runtime
        .processes_matching(&needles)?
        .into_iter()
        .filter(|process| is_syncdash_desktop_process(&process.name))
        .collect::<Vec<_>>();
    runtime.kill_processes(&processes, "this SyncDash checkout")?;
    Ok(())
}

fn is_syncdash_desktop_process(name: &str) -> bool {
    matches!(
        name.trim_end_matches(".exe").to_ascii_lowercase().as_str(),
        "syncdash-desktop" | "syncdash-dist" | "syncdash-max" | "syncdash-release"
    )
}

fn free_cli(runtime: &Runtime) -> BuildResult<()> {
    if runtime.dry_run() {
        runtime.print_dry_run("inspect running syncdash CLI and ask before stopping it");
        return Ok(());
    }
    let roots = [
        format!("{}/", runtime.path("target").display()),
        format!("{}/", runtime.project_artifact_root(PROJECT_ID)?.display()),
    ];
    let processes = runtime
        .processes_matching(&roots)?
        .into_iter()
        .filter(|process| {
            process
                .name
                .trim_end_matches(".exe")
                .eq_ignore_ascii_case("syncdash")
        })
        .collect::<Vec<_>>();
    if processes.is_empty() {
        return Ok(());
    }
    println!();
    for process in &processes {
        println!(
            "    syncdash  PID {}  {}",
            process.pid, process.command_line
        );
    }
    println!("  [Warn] A syncdash process is running; it may be mid-apply.");
    if runtime.prompt_yes_no("Kill it?", false)? {
        runtime.kill_processes(&processes, "syncdash CLI")?;
    } else {
        return Err(BuildError::new(
            "syncdash CLI was left running; refusing to replace its source or durable executable",
        ));
    }
    Ok(())
}

fn require_committed_dist(runtime: &Runtime) -> BuildResult<()> {
    runtime.require_directory(
        &runtime.path("dist"),
        "committed frontend output; run npm run build and commit dist/",
    )
}

fn dev(runtime: &Runtime) -> BuildResult<()> {
    runtime.ensure_node_modules(runtime.root())?;
    free_desktop(runtime)?;
    runtime.kill_port(VITE_PORT)?;
    println!();
    println!("  [Dev] npx tauri dev from Dev/ - Vite + Tauri");
    runtime.run(
        "npx",
        ["tauri", "dev"],
        &runtime.path("Dev"),
        &cargo_environment(runtime, &[]),
    )
}

fn build_tiers(runtime: &Runtime, tiers: &[Tier]) -> BuildResult<()> {
    if tiers.is_empty() {
        return Err(BuildError::new("at least one build tier is required"));
    }
    match runtime.host() {
        Host::Windows => runtime.ensure_node_modules(runtime.root())?,
        Host::Macos => require_committed_dist(runtime)?,
    }
    free_desktop(runtime)?;
    runtime.wait_unlocked(&desktop_binary(runtime))?;
    runtime.total(|| {
        let frontend_steps = usize::from(runtime.host() == Host::Windows);
        let step_count = frontend_steps + tiers.len();
        let mut step = 1;
        if runtime.host() == Host::Windows {
            frontend_step(runtime, &format!("[{step}/{step_count}]"))?;
            step += 1;
        }
        let mut results: Vec<(Tier, Duration, u64)> = Vec::new();
        for tier in tiers.iter().copied() {
            let environment = tier.release_environment(runtime.host());
            let started = std::time::Instant::now();
            desktop_step(
                runtime,
                &format!("[{step}/{step_count}]"),
                tier,
                &environment,
            )?;
            step += 1;
            let desktop_mb = pack_desktop_tier(runtime, tier)?;
            results.push((tier, started.elapsed(), desktop_mb));
        }

        println!();
        println!("  ===== SELECTED DESKTOP TIERS BUILT OK =====");
        for (tier, elapsed, desktop_mb) in results {
            println!(
                "  {:<9} {}  desktop {} MB",
                tier.output_label(),
                format_duration(elapsed),
                desktop_mb
            );
        }
        Ok(())
    })
}

fn build_cli(runtime: &Runtime) -> BuildResult<()> {
    runtime.require_program("cargo")?;
    free_cli(runtime)?;
    runtime.wait_unlocked(&cli_binary(runtime))?;
    let published_cli = runtime.artifact_path(
        PROJECT_ID,
        PathBuf::from("cli").join(format!("syncdash{}", runtime.host().exe_suffix())),
    )?;
    runtime.wait_unlocked(&published_cli)?;
    let environment = Tier::Dist.release_environment(runtime.host());
    runtime.total(|| {
        cli_step(runtime, "[1/1]", Tier::Dist, &environment)?;
        free_cli(runtime)?;
        runtime.wait_unlocked(&cli_binary(runtime))?;
        runtime.wait_unlocked(&published_cli)?;
        let published = runtime.publish_artifact_file(
            PROJECT_ID,
            &cli_binary(runtime),
            PathBuf::from("cli").join(format!("syncdash{}", runtime.host().exe_suffix())),
        )?;
        runtime.print_artifact(&published, "CLI OK - syncdash")
    })
}

fn frontend_step(runtime: &Runtime, counter: &str) -> BuildResult<()> {
    runtime
        .phase(&format!("{counter} FRONTEND - Vite build"), || {
            runtime.run("npm", ["run", "build"], runtime.root(), &[])
        })
        .map(|_| ())
}

fn desktop_step(
    runtime: &Runtime,
    counter: &str,
    tier: Tier,
    environment: &[(String, Option<String>)],
) -> BuildResult<()> {
    let environment = cargo_environment(runtime, environment);
    runtime
        .phase(
            &format!("{counter} CARGO - SyncDash {} Desktop", tier.output_label()),
            || {
                runtime.run(
                    "cargo",
                    ["build", "--release", "-p", "syncdash-desktop"],
                    runtime.root(),
                    &environment,
                )
            },
        )
        .map(|_| ())
}

fn cli_step(
    runtime: &Runtime,
    counter: &str,
    tier: Tier,
    environment: &[(String, Option<String>)],
) -> BuildResult<()> {
    let environment = cargo_environment(runtime, environment);
    runtime
        .phase(
            &format!("{counter} CARGO - SyncDash {} CLI", tier.output_label()),
            || {
                runtime.run(
                    "cargo",
                    ["build", "--release", "-p", "syncdash"],
                    runtime.root(),
                    &environment,
                )
            },
        )
        .map(|_| ())
}

fn pack_desktop_tier(runtime: &Runtime, tier: Tier) -> BuildResult<u64> {
    let relative_directory = PathBuf::from("tiers").join(tier.key());
    let source = desktop_binary(runtime);
    if runtime.host() == Host::Macos {
        runtime.run(
            "chmod",
            ["+x".as_ref(), source.as_os_str()],
            runtime.root(),
            &[],
        )?;
    }
    free_desktop(runtime)?;
    runtime.wait_unlocked(&source)?;
    let destination = tier_desktop_binary(runtime, tier)?;
    runtime.wait_unlocked(&destination)?;
    let desktop = runtime.publish_artifact_file(
        PROJECT_ID,
        &source,
        relative_directory.join(format!(
            "SyncDash-{}{}",
            tier.output_label(),
            runtime.host().exe_suffix()
        )),
    )?;
    let desktop_mb = runtime.file_size_mb(&desktop)?;
    runtime.print_artifact(
        &desktop,
        &format!("PACKAGED - SyncDash {}", tier.output_label()),
    )?;
    Ok(desktop_mb)
}

fn build_installer(runtime: &Runtime) -> BuildResult<()> {
    runtime.ensure_node_modules(runtime.root())?;
    require_installer_processes_stopped(runtime)?;
    require_installer_disk_images_unmounted(runtime)?;
    free_desktop(runtime)?;
    runtime.wait_unlocked(&desktop_binary(runtime))?;
    let environment = Tier::Release.release_environment(runtime.host());
    let environment = cargo_environment(runtime, &environment);
    let tauri_root = runtime.path("Dev");
    match runtime.host() {
        Host::Windows => runtime.total(|| {
            runtime.phase("INSTALLER - Vite bundle + exe + NSIS setup", || {
                runtime.run(
                    "npx",
                    ["tauri", "build", "--bundles", "nsis"],
                    &tauri_root,
                    &environment,
                )
            })?;
            let setup = newest_windows_setup(runtime, &release_dir(runtime).join("bundle/nsis"))?;
            let name = setup.file_name().ok_or_else(|| {
                BuildError::new(format!("installer has no file name: {}", setup.display()))
            })?;
            require_installer_processes_stopped(runtime)?;
            let relative = PathBuf::from("installers").join(name);
            let destination = runtime.artifact_path(PROJECT_ID, &relative)?;
            runtime.wait_unlocked(&destination)?;
            let published = runtime.publish_artifact_file(PROJECT_ID, &setup, relative)?;
            runtime.print_artifact(&published, "INSTALLER SUCCESS")?;
            runtime.reveal(&published)
        }),
        Host::Macos => runtime.total(|| {
            runtime.capture("xcode-select", ["-p"], runtime.root(), &[])?;
            runtime.phase("INSTALLER - tauri build (.app + .dmg)", || {
                runtime.run(
                    "npx",
                    ["tauri", "build", "--bundles", "app,dmg"],
                    &tauri_root,
                    &environment,
                )
            })?;
            let bundle = release_dir(runtime).join("bundle");
            let app = runtime.newest_entry(&bundle.join("macos"), ".app")?;
            let dmg = runtime.newest_entry(&bundle.join("dmg"), ".dmg")?;
            let app_name = app.file_name().ok_or_else(|| {
                BuildError::new(format!("app bundle has no file name: {}", app.display()))
            })?;
            let dmg_name = dmg.file_name().ok_or_else(|| {
                BuildError::new(format!("disk image has no file name: {}", dmg.display()))
            })?;
            require_installer_processes_stopped(runtime)?;
            require_installer_disk_images_unmounted(runtime)?;
            free_desktop(runtime)?;
            let installer_relative = Path::new("installers");
            let app_entry = PathBuf::from(app_name);
            let dmg_entry = PathBuf::from(dmg_name);
            let installer_destination = runtime.artifact_path(PROJECT_ID, installer_relative)?;
            let app_destination = installer_destination.join(&app_entry);
            runtime.wait_unlocked(&app_destination)?;
            let dmg_destination = installer_destination.join(&dmg_entry);
            runtime.wait_unlocked(&dmg_destination)?;
            let published_installer = runtime.publish_artifact_bundle(
                PROJECT_ID,
                installer_relative,
                &[
                    ArtifactBundleEntry {
                        source: &app,
                        relative: &app_entry,
                        kind: ArtifactSourceKind::Directory,
                    },
                    ArtifactBundleEntry {
                        source: &dmg,
                        relative: &dmg_entry,
                        kind: ArtifactSourceKind::File,
                    },
                ],
            )?;
            let published_dmg = published_installer.join(dmg_entry);
            runtime.print_artifact(&published_dmg, "INSTALLER SUCCESS")?;
            runtime.reveal(&published_dmg)
        }),
    }
}

fn build_app_self(runtime: &Runtime) -> BuildResult<()> {
    if runtime.host() != Host::Macos {
        return Err(BuildError::new("app-self is available only on macOS"));
    }
    runtime.ensure_node_modules(runtime.root())?;
    free_desktop(runtime)?;
    let environment = Tier::Max.release_environment(runtime.host());
    let environment = cargo_environment(runtime, &environment);
    let tauri_root = runtime.path("Dev");
    runtime.total(|| {
        runtime.phase("SELF-USE APP - tauri build (.app)", || {
            runtime.run(
                "npx",
                ["tauri", "build", "--bundles", "app"],
                &tauri_root,
                &environment,
            )
        })?;
        let app = runtime.newest_entry(&release_dir(runtime).join("bundle/macos"), ".app")?;
        let app_name = app.file_name().ok_or_else(|| {
            BuildError::new(format!("app bundle has no file name: {}", app.display()))
        })?;
        free_desktop(runtime)?;
        let relative = PathBuf::from("app-self").join(app_name);
        let destination = runtime.artifact_path(PROJECT_ID, &relative)?;
        runtime.wait_unlocked(&destination)?;
        runtime.publish_artifact_directory(PROJECT_ID, &app, relative)?;
        let installed = runtime.install_macos_app(&app, &[BUNDLE_ID])?;
        runtime.launch_macos_app(&installed)
    })
}

fn run_tier(runtime: &Runtime, value: &str) -> BuildResult<()> {
    let binary = if value.eq_ignore_ascii_case("dev") {
        if runtime.host() != Host::Windows {
            return Err(BuildError::new(
                "the macOS dev binary depends on its Vite server and cannot be relaunched standalone",
            ));
        }
        runtime.path("target/debug/syncdash-desktop.exe")
    } else {
        tier_desktop_binary(runtime, Tier::parse(value)?)?
    };
    free_desktop(runtime)?;
    runtime.launch(&binary)
}

fn kill_only(runtime: &Runtime) -> BuildResult<()> {
    free_desktop(runtime)?;
    runtime.kill_port(VITE_PORT)?;
    free_cli(runtime)
}

fn kill_and_unlock(runtime: &Runtime) -> BuildResult<()> {
    kill_only(runtime)?;
    match runtime.host() {
        Host::Windows => {
            runtime.wait_unlocked(&desktop_binary(runtime))?;
            runtime.wait_unlocked(&runtime.path("target/debug/syncdash-desktop.exe"))?;
            runtime.wait_unlocked(&cli_binary(runtime))?;
            for tier in [Tier::Dist, Tier::Max, Tier::Release] {
                runtime.wait_unlocked(&tier_desktop_binary(runtime, tier)?)?;
            }
            Ok(())
        }
        Host::Macos => {
            runtime.kill_installed_app(Path::new("/Applications/SyncDash.app"))?;
            let removed = runtime.purge_appledouble()?;
            println!("  Removed {removed} AppleDouble files.");
            Ok(())
        }
    }
}

fn clean(runtime: &Runtime, level: CleanLevel) -> BuildResult<()> {
    let manifest = cleanup_manifest(runtime)?;
    let plan = plan_cleanup(runtime, &manifest, level, CleanTargetSelection::Project)?;
    print_cleanup_plan(&plan);
    verify_cleanup_plan_authorization(&plan)?;
    if !plan.has_ready_targets() {
        execute_cleanup(runtime, &plan)?.require_success()?;
        return runtime.mark_cleanup_succeeded();
    }
    require_cleanup_preconditions(runtime, level)?;
    require_cleanup_process_safety(runtime, &plan, true)?;
    if !confirm_cleanup(runtime, &plan)? {
        println!("  Clean cancelled; no outputs were removed.");
        return Ok(());
    }

    require_cleanup_preconditions(runtime, level)?;
    require_cleanup_process_safety(runtime, &plan, true)?;
    release_cleanup_desktop(runtime, level)?;
    if level != CleanLevel::Purge {
        adopt_legacy_artifacts(runtime)?;
    }
    require_cleanup_preconditions(runtime, level)?;
    release_cleanup_desktop(runtime, level)?;
    require_cleanup_process_safety(runtime, &plan, false)?;
    let report = execute_cleanup(runtime, &plan)?;
    report.require_success()?;
    if runtime.dry_run() {
        println!("  Cleanup dry run complete; committed dist/ would be preserved.");
    } else {
        println!("  Clean complete; committed dist/ was preserved.");
    }
    runtime.mark_cleanup_succeeded()
}

fn require_cleanup_process_safety(
    runtime: &Runtime,
    plan: &CleanPlan,
    allow_desktop_processes: bool,
) -> BuildResult<()> {
    let needles = plan
        .targets
        .iter()
        .filter(|target| target.present)
        .map(|target| {
            if target.path.is_dir() {
                format!("{}/", target.path.display())
            } else {
                target.path.display().to_string()
            }
        })
        .collect::<Vec<_>>();
    if needles.is_empty() {
        return Ok(());
    }
    if runtime.dry_run() {
        runtime.print_dry_run(if allow_desktop_processes {
            "block unknown/CLI processes in SyncDash cleanup targets; allow exact desktop processes until scoped release"
        } else {
            "verify no process remains in SyncDash cleanup targets"
        });
        return Ok(());
    }
    let blockers = runtime
        .processes_matching(&needles)?
        .into_iter()
        .filter(|process| !allow_desktop_processes || !is_syncdash_desktop_process(&process.name))
        .collect::<Vec<_>>();
    if blockers.is_empty() {
        return Ok(());
    }
    let detail = blockers
        .iter()
        .map(|process| format!("{} (PID {})", process.name, process.pid))
        .collect::<Vec<_>>()
        .join(", ");
    Err(BuildError::new(format!(
        "SyncDash cleanup target is used by a non-desktop or still-running process: {detail}; stop it and retry"
    )))
}

fn adopt_legacy_artifacts(runtime: &Runtime) -> BuildResult<()> {
    for tier in [Tier::Dist, Tier::Max, Tier::Release] {
        let legacy = runtime.path("target/builder-tiers").join(tier.key());
        let desktop = legacy.join(format!(
            "SyncDash-{}{}",
            tier.output_label(),
            runtime.host().exe_suffix()
        ));
        if desktop.is_file() {
            runtime.adopt_artifact_file(
                PROJECT_ID,
                &desktop,
                PathBuf::from("tiers")
                    .join(tier.key())
                    .join(desktop.file_name().expect("known desktop file name")),
            )?;
        }
    }

    let cli = cli_binary(runtime);
    if cli.is_file() {
        runtime.adopt_artifact_file(
            PROJECT_ID,
            &cli,
            PathBuf::from("cli").join(format!("syncdash{}", runtime.host().exe_suffix())),
        )?;
    }

    let bundle = release_dir(runtime).join("bundle");
    match runtime.host() {
        Host::Windows => {
            let directory = bundle.join("nsis");
            for setup in direct_entries_with_suffix(&directory, ".exe", false)? {
                let name = setup.file_name().ok_or_else(|| {
                    BuildError::new(format!("installer has no file name: {}", setup.display()))
                })?;
                runtime.adopt_artifact_file(
                    PROJECT_ID,
                    &setup,
                    PathBuf::from("installers").join(name),
                )?;
            }
        }
        Host::Macos => {
            adopt_macos_installer_bundle(runtime, &bundle)?;
        }
    }
    Ok(())
}

fn adopt_macos_installer_bundle(runtime: &Runtime, bundle: &Path) -> BuildResult<()> {
    let apps = direct_candidate_paths(&bundle.join("macos"), ".app")?;
    let images = direct_candidate_paths(&bundle.join("dmg"), ".dmg")?;
    if apps.is_empty() && images.is_empty() {
        return Ok(());
    }
    let relative = Path::new("installers");
    let destination = runtime.artifact_path(PROJECT_ID, relative)?;
    if validate_existing_macos_installer(&destination, "published SyncDash installer")? {
        return Ok(());
    }
    let Some((app, image)) = require_single_pair(&apps, &images, "legacy SyncDash installer")?
    else {
        unreachable!("at least one legacy installer candidate exists");
    };
    require_real_entry(app, ArtifactSourceKind::Directory, "legacy app bundle")?;
    require_real_entry(image, ArtifactSourceKind::File, "legacy disk image")?;
    let app_name = direct_file_name(app, "legacy app bundle")?;
    let image_name = direct_file_name(image, "legacy disk image")?;

    runtime.adopt_artifact_bundle(
        PROJECT_ID,
        relative,
        &[
            ArtifactBundleEntry {
                source: app,
                relative: &app_name,
                kind: ArtifactSourceKind::Directory,
            },
            ArtifactBundleEntry {
                source: image,
                relative: &image_name,
                kind: ArtifactSourceKind::File,
            },
        ],
    )?;
    Ok(())
}

fn require_single_pair<'a>(
    first: &'a [PathBuf],
    second: &'a [PathBuf],
    label: &str,
) -> BuildResult<Option<(&'a Path, &'a Path)>> {
    match (first, second) {
        ([], []) => Ok(None),
        ([first], [second]) => Ok(Some((first, second))),
        _ => Err(BuildError::new(format!(
            "{label} must contain exactly one real .app and one real .dmg; found {} app bundle(s) and {} disk image(s)",
            first.len(),
            second.len()
        ))),
    }
}

fn direct_file_name(path: &Path, label: &str) -> BuildResult<PathBuf> {
    path.file_name()
        .map(PathBuf::from)
        .ok_or_else(|| BuildError::new(format!("{label} has no file name: {}", path.display())))
}

fn require_real_entry(path: &Path, expected: ArtifactSourceKind, label: &str) -> BuildResult<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        BuildError::new(format!(
            "failed to inspect {label} {}: {error}",
            path.display()
        ))
    })?;
    if metadata_is_link_or_reparse(&metadata) || !metadata_matches_kind(&metadata, expected) {
        return Err(BuildError::new(format!(
            "{label} must be a real {:?}: {}",
            expected,
            path.display()
        )));
    }
    Ok(())
}

fn validate_existing_macos_installer(destination: &Path, label: &str) -> BuildResult<bool> {
    let Some(entries) = real_directory_entries(destination, label)? else {
        return Ok(false);
    };
    let mut apps = 0usize;
    let mut images = 0usize;
    for (path, metadata) in entries {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                BuildError::new(format!(
                    "{label} contains a non-UTF-8 entry: {}",
                    path.display()
                ))
            })?;
        if name.ends_with(".app") {
            if !metadata.is_dir() {
                return Err(BuildError::new(format!(
                    "{label} .app entry must be a real directory: {}",
                    path.display()
                )));
            }
            apps += 1;
        } else if name.ends_with(".dmg") {
            if !metadata.is_file() {
                return Err(BuildError::new(format!(
                    "{label} .dmg entry must be a real file: {}",
                    path.display()
                )));
            }
            images += 1;
        }
    }
    if apps != 1 || images != 1 {
        return Err(BuildError::new(format!(
            "{label} must contain exactly one real direct .app and one real direct .dmg; found {apps} app bundle(s) and {images} disk image(s): {}",
            destination.display()
        )));
    }
    Ok(true)
}

fn real_directory_entries(
    directory: &Path,
    label: &str,
) -> BuildResult<Option<Vec<(PathBuf, fs::Metadata)>>> {
    let metadata = match fs::symlink_metadata(directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(BuildError::new(format!(
                "failed to inspect {label} {}: {error}",
                directory.display()
            )))
        }
    };
    if metadata_is_link_or_reparse(&metadata) || !metadata.is_dir() {
        return Err(BuildError::new(format!(
            "{label} must be a managed real directory: {}",
            directory.display()
        )));
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(directory).map_err(|error| {
        BuildError::new(format!(
            "failed to read {label} {}: {error}",
            directory.display()
        ))
    })? {
        let entry = entry.map_err(|error| {
            BuildError::new(format!(
                "failed to read {label} entry in {}: {error}",
                directory.display()
            ))
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            BuildError::new(format!(
                "failed to inspect {label} entry {}: {error}",
                path.display()
            ))
        })?;
        if metadata_is_link_or_reparse(&metadata) {
            return Err(BuildError::new(format!(
                "{label} contains a direct link or reparse point: {}",
                path.display()
            )));
        }
        entries.push((path, metadata));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(Some(entries))
}

fn direct_entries_with_suffix(
    directory: &Path,
    suffix: &str,
    expect_directory: bool,
) -> BuildResult<Vec<PathBuf>> {
    let container = match fs::symlink_metadata(directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(BuildError::new(format!(
                "failed to inspect artifact container {}: {error}",
                directory.display()
            )))
        }
    };
    if !container.is_dir() || metadata_is_link_or_reparse(&container) {
        return Err(BuildError::new(format!(
            "artifact container is not a real directory: {}",
            directory.display()
        )));
    }
    let mut matches = Vec::new();
    for entry in fs::read_dir(directory).map_err(|error| {
        BuildError::new(format!("failed to read {}: {error}", directory.display()))
    })? {
        let entry = entry.map_err(|error| {
            BuildError::new(format!(
                "failed to read {} entry: {error}",
                directory.display()
            ))
        })?;
        let path = entry.path();
        let matches_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(suffix));
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            BuildError::new(format!("failed to inspect {}: {error}", path.display()))
        })?;
        if matches_name {
            let expected = if expect_directory {
                ArtifactSourceKind::Directory
            } else {
                ArtifactSourceKind::File
            };
            if metadata_is_link_or_reparse(&metadata) || !metadata_matches_kind(&metadata, expected)
            {
                return Err(BuildError::new(format!(
                    "artifact candidate must be a real {:?}: {}",
                    expected,
                    path.display()
                )));
            }
            matches.push(path);
        }
    }
    matches.sort();
    Ok(matches)
}

fn direct_candidate_paths(directory: &Path, suffix: &str) -> BuildResult<Vec<PathBuf>> {
    let container = match fs::symlink_metadata(directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(BuildError::new(format!(
                "failed to inspect artifact container {}: {error}",
                directory.display()
            )))
        }
    };
    if !container.is_dir() || metadata_is_link_or_reparse(&container) {
        return Err(BuildError::new(format!(
            "artifact container is not a real directory: {}",
            directory.display()
        )));
    }
    let mut matches = Vec::new();
    for entry in fs::read_dir(directory).map_err(|error| {
        BuildError::new(format!("failed to read {}: {error}", directory.display()))
    })? {
        let entry = entry.map_err(|error| {
            BuildError::new(format!(
                "failed to read {} entry: {error}",
                directory.display()
            ))
        })?;
        let path = entry.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(suffix))
        {
            matches.push(path);
        }
    }
    matches.sort();
    Ok(matches)
}

fn metadata_matches_kind(metadata: &fs::Metadata, kind: ArtifactSourceKind) -> bool {
    match kind {
        ArtifactSourceKind::File => metadata.is_file(),
        ArtifactSourceKind::Directory => metadata.is_dir(),
    }
}

#[cfg(windows)]
fn metadata_is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn metadata_is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

fn require_cleanup_preconditions(runtime: &Runtime, level: CleanLevel) -> BuildResult<()> {
    require_cleanup_blockers_stopped(runtime, level == CleanLevel::Purge)?;
    if runtime.host() == Host::Macos {
        let mut disk_image_roots = vec![release_dir(runtime).join("bundle")];
        if level == CleanLevel::Purge {
            disk_image_roots.push(runtime.project_artifact_root(PROJECT_ID)?);
        }
        runtime.require_disk_images_unmounted(&disk_image_roots)?;
    }
    Ok(())
}

fn require_cleanup_blockers_stopped(
    runtime: &Runtime,
    include_published_artifacts: bool,
) -> BuildResult<()> {
    if runtime.dry_run() {
        runtime.print_dry_run("verify scoped SyncDash CLI and installer processes are stopped");
        return Ok(());
    }
    let mut roots = vec![format!("{}/", runtime.path("target").display())];
    if include_published_artifacts {
        roots.push(format!(
            "{}/",
            runtime.project_artifact_root(PROJECT_ID)?.display()
        ));
    }
    let blockers = runtime
        .processes_matching(&roots)?
        .into_iter()
        .filter(|process| {
            let name = process.name.trim_end_matches(".exe").to_ascii_lowercase();
            name == "syncdash" || name.contains("setup") || name.contains("installer")
        })
        .collect::<Vec<_>>();
    if blockers.is_empty() {
        return Ok(());
    }
    let detail = blockers
        .iter()
        .map(|process| format!("{} (PID {})", process.name, process.pid))
        .collect::<Vec<_>>()
        .join(", ");
    Err(BuildError::new(format!(
        "cleanup is blocked by scoped SyncDash CLI or installer process(es): {detail}; stop them and retry"
    )))
}

fn require_installer_processes_stopped(runtime: &Runtime) -> BuildResult<()> {
    if runtime.dry_run() {
        runtime.print_dry_run("verify scoped SyncDash installer processes are stopped");
        return Ok(());
    }
    let roots = [
        format!("{}/", release_dir(runtime).join("bundle").display()),
        format!("{}/", runtime.project_artifact_root(PROJECT_ID)?.display()),
    ];
    let blockers = runtime
        .processes_matching(&roots)?
        .into_iter()
        .filter(|process| {
            let name = process.name.trim_end_matches(".exe").to_ascii_lowercase();
            name.contains("setup") || name.contains("installer")
        })
        .collect::<Vec<_>>();
    if blockers.is_empty() {
        return Ok(());
    }
    let detail = blockers
        .iter()
        .map(|process| format!("{} (PID {})", process.name, process.pid))
        .collect::<Vec<_>>()
        .join(", ");
    Err(BuildError::new(format!(
        "SyncDash installer build is blocked by running installer process(es): {detail}; stop them and retry"
    )))
}

fn require_installer_disk_images_unmounted(runtime: &Runtime) -> BuildResult<()> {
    if runtime.host() != Host::Macos {
        return Ok(());
    }
    runtime.require_disk_images_unmounted(&[
        release_dir(runtime).join("bundle"),
        runtime.project_artifact_root(PROJECT_ID)?,
    ])
}

fn release_cleanup_desktop(runtime: &Runtime, level: CleanLevel) -> BuildResult<()> {
    let mut owned_roots = vec![
        format!("{}/", runtime.path("target").display()),
        format!("{}/", runtime.path("node_modules").display()),
    ];
    if level == CleanLevel::Purge {
        owned_roots.push(format!(
            "{}/",
            runtime.project_artifact_root(PROJECT_ID)?.display()
        ));
    }
    let candidates = runtime
        .processes_matching(&owned_roots)?
        .into_iter()
        .filter(|process| is_syncdash_desktop_process(&process.name))
        .collect::<Vec<_>>();
    runtime
        .kill_processes(&candidates, "SyncDash desktop cleanup targets")
        .map(|_| ())
}

fn reveal(runtime: &Runtime) -> BuildResult<()> {
    runtime.reveal(&runtime.project_artifact_root(PROJECT_ID)?)
}

fn info(runtime: &Runtime) -> BuildResult<()> {
    let extra = [
        InfoLine {
            suffix: "build cli",
            detail: "only CLI build path; never part of numbered tiers",
        },
        InfoLine {
            suffix: "app-self",
            detail: "macOS: build, verify, and install the self-use app",
        },
        InfoLine {
            suffix: "clean [build|deep|purge]",
            detail: "caches; + downloads; + published artifacts",
        },
    ];
    print_standard_info(runtime, PROJECT_NAME, &extra);
    Ok(())
}

fn doctor(runtime: &Runtime) -> BuildResult<()> {
    runtime.doctor_header(PROJECT_NAME);
    runtime.assert_file("Cargo.toml", "workspace manifest")?;
    runtime.assert_file("package.json", "frontend package")?;
    runtime.assert_file("Dev/src-tauri/Cargo.toml", "Tauri manifest")?;
    runtime.require_directory(&runtime.path("dist"), "committed frontend output")?;
    cleanup_manifest(runtime)?;
    println!(
        "  [ok] cleanup manifest: {}",
        runtime.path("tools/builder/cleanup.toml").display()
    );
    println!(
        "  [ok] cargo: {}",
        runtime.require_program("cargo")?.display()
    );
    println!(
        "  [ok] node:  {}",
        runtime.require_program("node")?.display()
    );
    println!(
        "  [ok] npm:   {}",
        runtime.require_program("npm")?.display()
    );
    println!(
        "  [ok] npx:   {}",
        runtime.require_program("npx")?.display()
    );
    println!(
        "  [ok] git:   {}",
        runtime.require_program("git")?.display()
    );
    println!("SyncDash builder doctor passed.");
    Ok(())
}

fn newest_windows_setup(runtime: &Runtime, directory: &Path) -> BuildResult<PathBuf> {
    runtime
        .newest_entry(directory, "-setup.exe")
        .or_else(|_| runtime.newest_entry(directory, ".exe"))
}

fn print_help() {
    println!(
        "SyncDash builder\n\n\
         Commands:\n\
           dev\n\
           build dist|max|release|12|13|23|123|installer  Desktop tiers\n\
           build desktop                 legacy alias for Dist\n\
           build cli                     standalone CLI only, Dist policy\n\
           run dev|dist|max|release\n\
           run desktop                   legacy alias for Dist\n\
           kill | unlock | clean [build|deep|purge] | doctor | info\n\
           app-self                      macOS only\n\
           reveal\n\n\
         Global flags:\n\
           --dry-run\n\
           --yes                            confirm a cleanup plan without prompting\n\
           --host windows|macos           with --dry-run only"
    );
}

#[cfg(test)]
mod tests {
    use super::{
        command_requires_operation_lock, direct_candidate_paths, is_syncdash_desktop_process,
        normalize_command, require_real_entry, require_single_pair,
        validate_existing_macos_installer,
    };
    use builder_core::ArtifactSourceKind;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "syncdash-artifact-adoption-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&root).unwrap();
            Self { root }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn automatic_process_release_is_product_only() {
        for name in [
            "syncdash-desktop.exe",
            "SyncDash-Dist.exe",
            "SyncDash-Max",
            "SyncDash-Release",
        ] {
            assert!(is_syncdash_desktop_process(name));
        }
        for name in [
            "syncdash.exe",
            "explorer.exe",
            "Code.exe",
            "node.exe",
            "setup.exe",
        ] {
            assert!(!is_syncdash_desktop_process(name));
        }
    }

    #[test]
    fn menu_aliases_map_to_stable_commands() {
        assert_eq!(normalize_command(&["4".to_owned()]), ["build", "installer"]);
        assert_eq!(normalize_command(&["123".to_owned()]), ["build", "123"]);
        assert_eq!(normalize_command(&["s".to_owned()]), ["run", "dist"]);
        assert_eq!(normalize_command(&["C".to_owned()]), ["clean"]);
        assert_eq!(
            normalize_command(&["CLEAN".to_owned(), "DEEP".to_owned()]),
            ["clean", "deep"]
        );
        assert_eq!(
            normalize_command(&["CLEAN".to_owned(), "PURGE".to_owned()]),
            ["clean", "purge"]
        );
    }

    #[test]
    fn state_changing_commands_require_the_operation_lock() {
        for action in ["dev", "build", "run", "kill", "unlock", "clean", "app-self"] {
            assert!(command_requires_operation_lock(&[action.to_owned()]));
        }
        for action in ["quit", "reveal", "doctor", "info", "help"] {
            assert!(!command_requires_operation_lock(&[action.to_owned()]));
        }
    }

    #[test]
    fn legacy_installer_generation_requires_exactly_one_app_and_dmg() {
        let apps = vec![PathBuf::from("SyncDash.app")];
        let images = vec![PathBuf::from("SyncDash.dmg")];
        assert!(require_single_pair(&[], &[], "installer")
            .unwrap()
            .is_none());
        assert!(require_single_pair(&apps, &images, "installer")
            .unwrap()
            .is_some());
        assert!(require_single_pair(&apps, &[], "installer").is_err());
        assert!(require_single_pair(
            &apps,
            &[images[0].clone(), PathBuf::from("Old.dmg")],
            "installer"
        )
        .is_err());
    }

    #[test]
    fn published_installer_requires_complete_directory_owned_shape() {
        let fixture = Fixture::new("installer-shape");
        let destination = fixture.root.join("installers");
        fs::create_dir_all(destination.join("SyncDash.app")).unwrap();
        fs::write(destination.join("SyncDash.dmg"), b"dmg").unwrap();
        assert!(validate_existing_macos_installer(&destination, "installer").unwrap());

        fs::write(destination.join(".DS_Store"), b"metadata").unwrap();
        assert!(validate_existing_macos_installer(&destination, "installer").unwrap());

        fs::create_dir_all(destination.join("Old.app")).unwrap();
        assert!(validate_existing_macos_installer(&destination, "installer")
            .unwrap_err()
            .to_string()
            .contains("exactly one"));
    }

    #[test]
    fn published_installer_rejects_a_partial_generation() {
        let fixture = Fixture::new("installer-partial");
        let destination = fixture.root.join("installers");
        fs::create_dir_all(destination.join("SyncDash.app")).unwrap();
        assert!(validate_existing_macos_installer(&destination, "installer")
            .unwrap_err()
            .to_string()
            .contains("exactly one"));
    }

    #[test]
    fn matching_source_candidate_with_wrong_kind_fails_closed() {
        let fixture = Fixture::new("source-kind");
        fs::write(fixture.root.join("Fake.app"), b"not a directory").unwrap();
        let candidates = direct_candidate_paths(&fixture.root, ".app").unwrap();
        let error = require_real_entry(&candidates[0], ArtifactSourceKind::Directory, "legacy app")
            .unwrap_err();
        assert!(error.to_string().contains("must be a real Directory"));
    }

    #[cfg(unix)]
    #[test]
    fn linked_source_candidate_fails_closed() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new("source-link");
        let outside = fixture.root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, fixture.root.join("Linked.app")).unwrap();
        let candidates = direct_candidate_paths(&fixture.root, ".app").unwrap();
        assert!(
            require_real_entry(&candidates[0], ArtifactSourceKind::Directory, "legacy app")
                .is_err()
        );
    }
}
