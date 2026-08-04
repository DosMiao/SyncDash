use crate::cli::args::Cmd;

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn FreeConsole() -> i32;
}

fn detach_console() {
    #[cfg(windows)]
    unsafe {
        FreeConsole();
    }
}

fn launch_desktop() -> std::io::Result<i32> {
    use syncdash::foundation::host::HostOs;

    let executable_name = match HostOs::CURRENT {
        HostOs::Windows => "syncdash-desktop.exe",
        HostOs::MacOs | HostOs::Linux => "syncdash-desktop",
    };
    let candidate = std::env::current_exe().ok().and_then(|path| {
        path.parent()
            .map(|directory| directory.join(executable_name))
    });
    match candidate {
        Some(path) if path.exists() => {
            std::process::Command::new(path).spawn()?;
            Ok(0)
        }
        _ => {
            eprintln!("desktop app not found next to this binary ({executable_name}).");
            eprintln!("build it with: cargo build -p syncdash-desktop --release");
            eprintln!("(CLI subcommands all still work — run `syncdash --help`)");
            Ok(2)
        }
    }
}

pub(super) fn execute(command: Cmd) -> std::io::Result<i32> {
    match command {
        Cmd::Gui => launch_desktop(),
        _ => unreachable!("desktop handler received another command"),
    }
}

/// A command-less Windows launch is the double-click path, so release its transient console.
pub(super) fn launch_default() -> std::io::Result<i32> {
    detach_console();
    launch_desktop()
}
