//! Go/no-go for a native SMB backend: can a pure-Rust SMB2 client do the job against a real
//! server, before any of the `Vfs` surface is built on top of it?
//!
//! The backend behind `smb://` delegates to the OS today — a UNC path on Windows, three
//! subprocesses on macOS, and nothing at all on Linux. Replacing it with an in-process client is
//! the last piece of "every transport is Rust", but it is also the riskiest: the available crates
//! are pre-1.0, and a sync tool that silently mis-reads a share is worse than one that refuses.
//!
//! So this probes the four things the `Vfs` trait actually needs — connect, list, write, read back
//! — and nothing else. If it cannot do these against a live server, no backend written on it can.
//!
//! # Running it
//!
//! It needs a server, so it is ignored by default. The password is **not** an argument and not an
//! environment variable: it comes from the same OS credential store the rest of the tool uses, so
//! it never appears in a shell history, a process listing or a CI log.
//!
//! ```text
//! syncdash cred set "smb://<user>@<host>/<share>"      # prompts, stores in the OS keychain
//! set SYNCDASH_SMB_URL=smb://<user>@<host>/<share>/<subdir>
//! cargo test --test smb_native_spike -- --ignored --nocapture
//! ```
//!
//! A pass justifies writing the backend. A failure is a no-go and costs nothing: `\\host\share`
//! keeps working, because it has always parsed as a plain local path.

use syncdash::fs::vfs::cred;
use syncdash::fs::vfs::spec::{parse, RootSpec};

/// The four operations a `Vfs` backend cannot be built without.
#[test]
#[ignore = "needs a live SMB server; see the module docs"]
fn a_pure_rust_client_can_drive_a_real_share() {
    let url = std::env::var("SYNCDASH_SMB_URL")
        .expect("set SYNCDASH_SMB_URL to an smb://user@host/share/subdir phrase");
    let RootSpec::Remote(spec) = parse(&url) else {
        panic!("'{url}' is not an smb:// phrase");
    };
    assert_eq!(spec.scheme, "smb", "this spike is about smb://");
    let user = spec.user.clone().expect("the phrase must name a user — NTLM has no anonymous mode");

    // Same account key the real backend derives, so one `cred set` serves both.
    let account = cred::account_for(&spec).expect("a phrase with a user has an account");
    let password = cred::get_secret(&account)
        .expect("credential store readable")
        .unwrap_or_else(|| panic!("no stored secret for {account} — run: syncdash cred set \"{url}\""));

    let port = spec.port.unwrap_or(445);
    let addr = format!("{}:{}", spec.host, port);
    // `root` is "<share>/<subdir…>": the first segment is the share, the rest is the path in it.
    let (share, sub) = spec.root.split_once('/').unwrap_or((spec.root.as_str(), ""));
    assert!(!share.is_empty(), "'{url}' names no share");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_io()
        .enable_time()
        .build()
        .expect("tokio runtime");

    rt.block_on(async {
        let mut client = smb2::connect(&addr, &user, &password)
            .await
            .unwrap_or_else(|e| panic!("connect to {addr} as {user} failed: {e}"));
        println!("  connect        ok  ({addr} as {user})");

        let mut tree = client
            .connect_share(share)
            .await
            .unwrap_or_else(|e| panic!("connect_share '{share}' failed: {e}"));
        println!("  connect_share  ok  ({share})");

        let dir = if sub.is_empty() { String::new() } else { format!("{sub}/") };
        let entries = client
            .list_directory(&mut tree, &dir)
            .await
            .unwrap_or_else(|e| panic!("list_directory '{dir}' failed: {e}"));
        println!("  list_directory ok  ({} entries under '{dir}')", entries.len());

        // Write and read back through the client itself — reading a file some other tool wrote
        // would not prove the write path works, and the write path is where a sync tool does harm.
        let rel = if sub.is_empty() {
            "syncdash-spike.bin".to_string()
        } else {
            format!("{sub}/syncdash-spike.bin")
        };
        let payload: Vec<u8> = (0..64u32).flat_map(|i| i.to_le_bytes()).collect();
        client
            .write_file(&mut tree, &rel, &payload)
            .await
            .unwrap_or_else(|e| panic!("write_file '{rel}' failed: {e}"));
        println!("  write_file     ok  ({} bytes)", payload.len());

        let got = client
            .read_file(&mut tree, &rel)
            .await
            .unwrap_or_else(|e| panic!("read_file '{rel}' failed: {e}"));
        assert_eq!(got, payload, "what came back is not what went out");
        println!("  read_file      ok  (round-trips byte for byte)");

        // Leave nothing behind; a failure here is worth knowing about too, since delete is one of
        // the operations the backend needs.
        match client.delete_file(&mut tree, &rel).await {
            Ok(()) => println!("  delete_file    ok"),
            Err(e) => panic!("delete_file '{rel}' failed, leaving {rel} behind: {e}"),
        }
    });

    println!("\nGO: a pure-Rust SMB2 client drives this server. The backend can be built on it.");
}
