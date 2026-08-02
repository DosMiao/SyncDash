use crate::features::autoscan::model::AutoScanVerificationTerminal;

pub(in crate::features::operations) fn format_run_io_error(error: std::io::Error) -> String {
    if syncdash::obs::progress::is_cancelled(&error) {
        "cancelled".into()
    } else {
        error.to_string()
    }
}

pub(in crate::features::operations) fn verification_terminal_from_io(
    error: &std::io::Error,
) -> AutoScanVerificationTerminal {
    if syncdash::obs::progress::is_cancelled(error) {
        AutoScanVerificationTerminal::Cancelled
    } else {
        AutoScanVerificationTerminal::Failed(error.to_string())
    }
}
