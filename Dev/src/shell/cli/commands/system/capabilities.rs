use crate::cli::args::Cmd;
use syncdash::run;

pub(super) fn execute(command: Cmd) -> std::io::Result<i32> {
    match command {
        Cmd::Caps { phrase } => match run::describe_root(&phrase) {
            Ok(sheet) => {
                println!("{sheet}");
                Ok(0)
            }
            Err(e) => {
                eprintln!("connect failed: {e}");
                Ok(1)
            }
        },
        _ => unreachable!("capabilities handler received another command"),
    }
}
