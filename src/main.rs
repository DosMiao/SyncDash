//! The `syncdash` command-line binary.
//!
//! Argument surface and dispatch live in `cli`; this file is the entry point and nothing else.

mod cli;

use std::sync::Arc;

use clap::Parser;

use cli::args::Cli;

fn main() {
    // Keep the process sink installed until CLI shutdown.
    let _session = syncdash::boot::init(|cfg| {
        cfg.mirror_stderr.then(|| {
            Arc::new(syncdash::obs::logging::StderrSink {
                min_level: cfg.level,
            }) as Arc<_>
        })
    });
    let cli = Cli::parse();
    let code = match cli::run_cli(cli) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            2
        }
    };
    std::process::exit(code);
}
