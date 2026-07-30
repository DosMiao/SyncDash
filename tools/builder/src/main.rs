use std::path::{Path, PathBuf};
use std::process::ExitCode;

use builder_core::{
    parse_common_args, project_root_from_manifest, BuildError, BuildResult, Host, Menu, MenuEntry,
    MenuSection, Runtime,
};

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

    match command.as_slice() {
        [action] if action == "quit" => Ok(()),
        [action] if action == "dev" => dev(&runtime),
        [action, target] if action == "build" && target == "desktop" => build_desktop(&runtime),
        [action, target] if action == "build" && target == "cli" => build_cli(&runtime),
        [action, target] if action == "build" && target == "all" => build_all(&runtime),
        [action, target] if action == "build" && target == "installer" => build_installer(&runtime),
        [action, target] if action == "run" && target == "desktop" => run_desktop(&runtime),
        [action] if action == "kill" => kill_only(&runtime),
        [action] if action == "unlock" => kill_and_unlock(&runtime),
        [action] if action == "clean" => clean(&runtime),
        [action] if action == "app-self" => build_app_self(&runtime),
        [action] if action == "reveal" => reveal(&runtime),
        [action] if action == "doctor" => doctor(&runtime),
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
    let build = [
        MenuEntry {
            key: "1",
            label: "Dev",
            detail: "tauri dev, HMR",
        },
        MenuEntry {
            key: "2",
            label: "Desktop",
            detail: "GUI release",
        },
        MenuEntry {
            key: "3",
            label: "CLI",
            detail: "syncdash, no Node",
        },
        MenuEntry {
            key: "4",
            label: "All",
            detail: "Desktop + CLI",
        },
        MenuEntry {
            key: "5",
            label: "Installer",
            detail: "NSIS or .app + .dmg",
        },
    ];
    let bundle = [
        MenuEntry {
            key: "A",
            label: "App Self",
            detail: ".app -> /Applications",
        },
        MenuEntry {
            key: "V",
            label: "Reveal",
            detail: "open bundle output",
        },
    ];
    let utility = [
        MenuEntry {
            key: "R",
            label: "Run Desktop",
            detail: "kill + launch",
        },
        MenuEntry {
            key: "6",
            label: "Kill",
            detail: "desktop + CLI + dev port",
        },
        MenuEntry {
            key: "7",
            label: "Unlock",
            detail: "kill + release locks",
        },
        MenuEntry {
            key: "8",
            label: "Clean",
            detail: "cargo target; keep dist/",
        },
        MenuEntry {
            key: "Q",
            label: "Quit",
            detail: "",
        },
    ];
    let sections = if runtime.host() == Host::Macos {
        vec![
            MenuSection {
                title: "Build",
                entries: &build,
            },
            MenuSection {
                title: "Bundle",
                entries: &bundle,
            },
            MenuSection {
                title: "Utility",
                entries: &utility,
            },
        ]
    } else {
        vec![
            MenuSection {
                title: "Build",
                entries: &build,
            },
            MenuSection {
                title: "Utility",
                entries: &utility,
            },
        ]
    };
    let choice = Menu {
        title: "SyncDash Builder",
        root: runtime.root(),
        sections: &sections,
        prompt: "Choice [1-8 R Q]",
        default: "2",
    }
    .choose()?;
    Ok(normalize_command(&[choice]))
}

fn normalize_command(input: &[String]) -> Vec<String> {
    let command: Vec<String> = input.iter().map(|part| part.to_ascii_lowercase()).collect();
    let normalized: &[&str] = match command.as_slice() {
        [value] if value == "1" => &["dev"],
        [value] if value == "2" => &["build", "desktop"],
        [value] if value == "3" => &["build", "cli"],
        [value] if value == "4" => &["build", "all"],
        [value] if value == "5" => &["build", "installer"],
        [value] if value == "6" => &["kill"],
        [value] if value == "7" => &["unlock"],
        [value] if value == "8" => &["clean"],
        [value] if value == "r" => &["run", "desktop"],
        [value] if value == "a" => &["app-self"],
        [value] if value == "v" => &["reveal"],
        [value] if value == "q" => &["quit"],
        _ => return command,
    };
    normalized.iter().map(|part| (*part).to_owned()).collect()
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

fn free_desktop(runtime: &Runtime) -> BuildResult<()> {
    println!();
    println!("  [Kill] Freeing syncdash-desktop ...");
    match runtime.host() {
        Host::Windows => {
            runtime.kill_named(&["syncdash-desktop"], "syncdash-desktop")?;
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
    println!("  [Dev] npx tauri dev - Vite + Tauri");
    runtime.run("npx", ["tauri", "dev"], runtime.root(), &[])
}

fn build_desktop(runtime: &Runtime) -> BuildResult<()> {
    match runtime.host() {
        Host::Windows => runtime.ensure_node_modules(runtime.root())?,
        Host::Macos => require_committed_dist(runtime)?,
    }
    free_desktop(runtime)?;
    runtime.wait_unlocked(&desktop_binary(runtime))?;
    runtime.total(|| {
        if runtime.host() == Host::Windows {
            frontend_step(runtime, "[1/2]")?;
            desktop_step(runtime, "[2/2]")?;
        } else {
            desktop_step(runtime, "[1/1]")?;
        }
        runtime.print_artifact(&desktop_binary(runtime), "DESKTOP OK - syncdash-desktop")?;
        runtime.launch(&desktop_binary(runtime))
    })
}

fn build_cli(runtime: &Runtime) -> BuildResult<()> {
    runtime.require_program("cargo")?;
    free_cli(runtime)?;
    runtime.wait_unlocked(&cli_binary(runtime))?;
    runtime.total(|| {
        cli_step(runtime, "[1/1]")?;
        runtime.print_artifact(&cli_binary(runtime), "CLI OK - syncdash")
    })
}

fn build_all(runtime: &Runtime) -> BuildResult<()> {
    match runtime.host() {
        Host::Windows => runtime.ensure_node_modules(runtime.root())?,
        Host::Macos => require_committed_dist(runtime)?,
    }
    free_desktop(runtime)?;
    free_cli(runtime)?;
    runtime.wait_unlocked(&desktop_binary(runtime))?;
    runtime.wait_unlocked(&cli_binary(runtime))?;
    runtime.total(|| {
        if runtime.host() == Host::Windows {
            frontend_step(runtime, "[1/3]")?;
            desktop_step(runtime, "[2/3]")?;
            cli_step(runtime, "[3/3]")?;
        } else {
            desktop_step(runtime, "[1/2]")?;
            cli_step(runtime, "[2/2]")?;
        }
        runtime.print_artifact(&desktop_binary(runtime), "DESKTOP OK - syncdash-desktop")?;
        runtime.print_artifact(&cli_binary(runtime), "CLI OK - syncdash")?;
        runtime.launch(&desktop_binary(runtime))
    })
}

fn frontend_step(runtime: &Runtime, counter: &str) -> BuildResult<()> {
    runtime
        .phase(&format!("{counter} FRONTEND - Vite build"), || {
            runtime.run("npm", ["run", "build"], runtime.root(), &[])
        })
        .map(|_| ())
}

fn desktop_step(runtime: &Runtime, counter: &str) -> BuildResult<()> {
    runtime
        .phase(
            &format!("{counter} CARGO - syncdash-desktop, release"),
            || {
                runtime.run(
                    "cargo",
                    ["build", "--release", "-p", "syncdash-desktop"],
                    runtime.root(),
                    &[],
                )
            },
        )
        .map(|_| ())
}

fn cli_step(runtime: &Runtime, counter: &str) -> BuildResult<()> {
    runtime
        .phase(&format!("{counter} CARGO - syncdash, release"), || {
            runtime.run(
                "cargo",
                ["build", "--release", "-p", "syncdash"],
                runtime.root(),
                &[],
            )
        })
        .map(|_| ())
}

fn build_installer(runtime: &Runtime) -> BuildResult<()> {
    runtime.ensure_node_modules(runtime.root())?;
    free_desktop(runtime)?;
    runtime.wait_unlocked(&desktop_binary(runtime))?;
    match runtime.host() {
        Host::Windows => runtime.total(|| {
            runtime.phase("INSTALLER - Vite bundle + exe + NSIS setup", || {
                runtime.run(
                    "npx",
                    ["tauri", "build", "--bundles", "nsis"],
                    runtime.root(),
                    &[],
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
                    runtime.root(),
                    &[],
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
    runtime.total(|| {
        runtime.phase("SELF-USE APP - tauri build (.app)", || {
            runtime.run(
                "npx",
                ["tauri", "build", "--bundles", "app"],
                runtime.root(),
                &[],
            )
        })?;
        let app = runtime.newest_entry(&release_dir(runtime).join("bundle/macos"), ".app")?;
        let installed = runtime.install_macos_app(&app, &[BUNDLE_ID])?;
        runtime.run("open", [installed.as_os_str()], runtime.root(), &[])
    })
}

fn run_desktop(runtime: &Runtime) -> BuildResult<()> {
    free_desktop(runtime)?;
    runtime.launch(&desktop_binary(runtime))
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
            runtime.wait_unlocked(&cli_binary(runtime))
        }
        Host::Macos => {
            runtime.kill_installed_app(Path::new("/Applications/SyncDash.app"))?;
            Ok(())
        }
    }
}

fn clean(runtime: &Runtime) -> BuildResult<()> {
    free_desktop(runtime)?;
    free_cli(runtime)?;
    runtime.run(
        "cargo",
        ["clean", "--manifest-path", "Cargo.toml"],
        runtime.root(),
        &[],
    )?;
    println!("  Clean complete; committed dist/ was preserved.");
    Ok(())
}

fn reveal(runtime: &Runtime) -> BuildResult<()> {
    if runtime.host() != Host::Macos {
        return Err(BuildError::new("reveal is available only on macOS"));
    }
    let bundle = release_dir(runtime).join("bundle");
    let selected = if bundle.join("dmg").is_dir() {
        bundle.join("dmg")
    } else {
        bundle.join("macos")
    };
    runtime.reveal(&selected)
}

fn doctor(runtime: &Runtime) -> BuildResult<()> {
    runtime.doctor_header(PROJECT_NAME);
    runtime.assert_file("Cargo.toml", "workspace manifest")?;
    runtime.assert_file("package.json", "frontend package")?;
    runtime.assert_file("src-tauri/Cargo.toml", "Tauri manifest")?;
    runtime.require_directory(&runtime.path("dist"), "committed frontend output")?;
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
           build desktop|cli|all|installer\n\
           run desktop\n\
           kill | unlock | clean | doctor\n\
           app-self | reveal             macOS only\n\n\
         Global flags:\n\
           --dry-run\n\
           --host windows|macos           with --dry-run only"
    );
}

#[cfg(test)]
mod tests {
    use super::normalize_command;

    #[test]
    fn menu_aliases_map_to_stable_commands() {
        assert_eq!(normalize_command(&["4".to_owned()]), ["build", "all"]);
        assert_eq!(normalize_command(&["r".to_owned()]), ["run", "desktop"]);
    }
}
