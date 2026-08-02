use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use builder_core::{
    confirm_cleanup, execute_cleanup, format_duration, load_cleanup_manifest,
    normalize_standard_command, parse_common_args, parse_tier_selection, plan_cleanup,
    print_cleanup_plan, print_standard_info, project_root_from_manifest, standard_project_command,
    verify_cleanup_plan_authorization, BuildError, BuildResult, BuildTier as Tier, CleanLevel,
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
    let root = project_root_from_manifest(env!("CARGO_MANIFEST_DIR"))?;
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

fn tier_root(runtime: &Runtime) -> PathBuf {
    runtime.path("target/builder-tiers")
}

fn tier_dir(runtime: &Runtime, tier: Tier) -> PathBuf {
    tier_root(runtime).join(tier.key())
}

fn tier_desktop_binary(runtime: &Runtime, tier: Tier) -> PathBuf {
    tier_dir(runtime, tier).join(format!(
        "SyncDash-{}{}",
        tier.output_label(),
        runtime.host().exe_suffix()
    ))
}

fn free_desktop(runtime: &Runtime) -> BuildResult<()> {
    println!();
    println!("  [Kill] Freeing syncdash-desktop ...");
    match runtime.host() {
        Host::Windows => {
            runtime.kill_named(
                &[
                    "syncdash-desktop",
                    "syncdash-dist",
                    "syncdash-max",
                    "syncdash-release",
                ],
                "syncdash-desktop",
            )?;
        }
        Host::Macos => {
            let needles = [
                desktop_binary(runtime).display().to_string(),
                runtime
                    .path("target/debug/syncdash-desktop")
                    .display()
                    .to_string(),
                runtime.path("node_modules").display().to_string(),
            ];
            runtime.kill_matching(&needles, "syncdash-desktop")?;
        }
    }
    Ok(())
}

fn free_cli(runtime: &Runtime) -> BuildResult<()> {
    if runtime.dry_run() {
        runtime.print_dry_run("inspect running syncdash CLI and ask before stopping it");
        return Ok(());
    }
    let processes = runtime.processes_named(&["syncdash"])?;
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
        println!("    Left running; a Windows build will fail if the executable remains locked.");
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
    runtime.run("npx", ["tauri", "dev"], &runtime.path("Dev"), &[])
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
    let environment = Tier::Dist.release_environment(runtime.host());
    runtime.total(|| {
        cli_step(runtime, "[1/1]", Tier::Dist, &environment)?;
        runtime.print_artifact(&cli_binary(runtime), "CLI OK - syncdash")
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
    runtime
        .phase(
            &format!("{counter} CARGO - SyncDash {} Desktop", tier.output_label()),
            || {
                runtime.run(
                    "cargo",
                    ["build", "--release", "-p", "syncdash-desktop"],
                    runtime.root(),
                    environment,
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
    runtime
        .phase(
            &format!("{counter} CARGO - SyncDash {} CLI", tier.output_label()),
            || {
                runtime.run(
                    "cargo",
                    ["build", "--release", "-p", "syncdash"],
                    runtime.root(),
                    environment,
                )
            },
        )
        .map(|_| ())
}

fn pack_desktop_tier(runtime: &Runtime, tier: Tier) -> BuildResult<u64> {
    let directory = tier_dir(runtime, tier);
    runtime.remove_directory(&directory)?;
    runtime.create_directory(&directory)?;
    let desktop = tier_desktop_binary(runtime, tier);
    runtime.copy_file(&desktop_binary(runtime), &desktop)?;
    if runtime.host() == Host::Macos {
        runtime.run(
            "chmod",
            ["+x".as_ref(), desktop.as_os_str()],
            runtime.root(),
            &[],
        )?;
    }
    let desktop_mb = runtime.file_size_mb(&desktop)?;
    runtime.print_artifact(
        &desktop,
        &format!("PACKAGED - SyncDash {}", tier.output_label()),
    )?;
    Ok(desktop_mb)
}

fn build_installer(runtime: &Runtime) -> BuildResult<()> {
    runtime.ensure_node_modules(runtime.root())?;
    free_desktop(runtime)?;
    runtime.wait_unlocked(&desktop_binary(runtime))?;
    let environment = Tier::Release.release_environment(runtime.host());
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
            runtime.print_artifact(&setup, "INSTALLER SUCCESS")?;
            runtime.reveal(&setup)
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
            let dmg = runtime.newest_entry(&bundle.join("dmg"), ".dmg")?;
            runtime.print_artifact(&dmg, "INSTALLER SUCCESS")?;
            runtime.reveal(&dmg)
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
        let installed = runtime.install_macos_app(&app, &[BUNDLE_ID])?;
        runtime.run("open", [installed.as_os_str()], runtime.root(), &[])
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
        tier_desktop_binary(runtime, Tier::parse(value)?)
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
                runtime.wait_unlocked(&tier_desktop_binary(runtime, tier))?;
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
        return execute_cleanup(runtime, &plan)?.require_success();
    }
    if !confirm_cleanup(runtime, &plan)? {
        println!("  Clean cancelled; no outputs were removed.");
        return Ok(());
    }

    free_desktop(runtime)?;
    free_cli(runtime)?;
    let report = execute_cleanup(runtime, &plan)?;
    report.require_success()?;
    if runtime.dry_run() {
        println!("  Cleanup dry run complete; committed dist/ would be preserved.");
    } else {
        println!("  Clean complete; committed dist/ was preserved.");
    }
    Ok(())
}

fn reveal(runtime: &Runtime) -> BuildResult<()> {
    let bundle = release_dir(runtime).join("bundle");
    let selected = if bundle.join("dmg").is_dir() {
        bundle.join("dmg")
    } else if tier_root(runtime).is_dir() {
        tier_root(runtime)
    } else {
        release_dir(runtime)
    };
    runtime.reveal(&selected)
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
            suffix: "clean [build|deep]",
            detail: "remove declared rebuildable outputs while preserving committed dist/",
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
           kill | unlock | clean [build|deep] | doctor | info\n\
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
    use super::{command_requires_operation_lock, normalize_command};

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
}
