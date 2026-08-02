use crate::contracts::operations::PostRunPowerActionDto;

pub(crate) fn launch(action: PostRunPowerActionDto) -> Result<(), String> {
    #[cfg(windows)]
    let (program, arguments): (&str, Vec<&str>) = match action {
        PostRunPowerActionDto::Sleep => (
            "rundll32.exe",
            vec!["powrprof.dll,SetSuspendState", "0,1,0"],
        ),
        PostRunPowerActionDto::Shutdown => ("shutdown", vec!["/s", "/t", "5"]),
    };
    #[cfg(target_os = "macos")]
    let (program, arguments): (&str, Vec<&str>) = match action {
        PostRunPowerActionDto::Sleep => ("pmset", vec!["sleepnow"]),
        PostRunPowerActionDto::Shutdown => (
            "osascript",
            vec!["-e", "tell application \"System Events\" to shut down"],
        ),
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let (program, arguments): (&str, Vec<&str>) = match action {
        PostRunPowerActionDto::Sleep => ("systemctl", vec!["suspend"]),
        PostRunPowerActionDto::Shutdown => ("systemctl", vec!["poweroff"]),
    };
    std::process::Command::new(program)
        .args(&arguments)
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}
