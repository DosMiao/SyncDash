use crate::cli::args::{Cmd, CredCmd};

pub(super) fn execute(command: Cmd) -> std::io::Result<i32> {
    match command {
        Cmd::Cred { cmd } => {
            use syncdash::fs::vfs::cred;
            use syncdash::fs::vfs::spec::{parse, RootSpec};
            match cmd {
                CredCmd::Set { phrase, stdin } => {
                    let RootSpec::Endpoint(r) = parse(&phrase) else {
                        eprintln!(
                            "not a network endpoint phrase: {phrase} (expected scheme://user@host/...)"
                        );
                        return Ok(2);
                    };
                    let Some(acc) = cred::account_for(&r) else {
                        eprintln!("the phrase names no user — spell it scheme://user@host/... so the credential has an owner");
                        return Ok(2);
                    };
                    let pw = if stdin {
                        use std::io::Read as _;
                        let mut s = String::new();
                        std::io::stdin().read_to_string(&mut s)?;
                        s
                    } else {
                        rpassword::prompt_password(format!("password for {acc}: "))?
                    };
                    let pw = pw.trim_end_matches(['\r', '\n']);
                    if pw.is_empty() {
                        eprintln!("empty password — nothing stored");
                        return Ok(2);
                    }
                    cred::set_secret(&acc, pw).map_err(std::io::Error::from)?;
                    println!("stored in the OS credential store: {acc}");
                    Ok(0)
                }
                CredCmd::Rm { phrase } => {
                    let RootSpec::Endpoint(r) = parse(&phrase) else {
                        eprintln!("not a network endpoint phrase: {phrase}");
                        return Ok(2);
                    };
                    let Some(acc) = cred::account_for(&r) else {
                        eprintln!("the phrase names no user");
                        return Ok(2);
                    };
                    if cred::delete_secret(&acc).map_err(std::io::Error::from)? {
                        println!("removed: {acc}");
                    } else {
                        println!("no entry stored for {acc}");
                    }
                    Ok(0)
                }
                CredCmd::Ls => {
                    let accounts = cred::list_accounts();
                    if accounts.is_empty() {
                        println!("no stored credentials (add one with: syncdash cred set \"smb://user@host/share\")");
                    }
                    for a in accounts {
                        println!("{a}");
                    }
                    Ok(0)
                }
                CredCmd::Test { phrase } => {
                    let v = syncdash::fs::vfs::open(&phrase, &cred::default_provider())
                        .map_err(std::io::Error::from)?;
                    match v.connect() {
                        Ok(()) => {
                            println!("connected: {}", v.display());
                            if let Some(info) = v.server_info() {
                                println!("  {info}");
                            }
                            let c = v.caps();
                            println!(
                                "  protocol {}, mtime precision {} ms, up to {} parallel stream(s)",
                                c.protocol, c.mtime_precision_ms, c.max_parallel_streams
                            );
                            Ok(0)
                        }
                        Err(e) => {
                            eprintln!("connect failed: {e}");
                            Ok(1)
                        }
                    }
                }
            }
        }
        _ => unreachable!("credential handler received another command"),
    }
}
