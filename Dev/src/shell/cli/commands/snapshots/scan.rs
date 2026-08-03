use crate::cli::{args::Cmd, write_out};
use syncdash::job::{self, junk};
use syncdash::pipeline::{filter, scan};

pub(super) fn execute(command: Cmd) -> std::io::Result<i32> {
    match command {
        Cmd::Scan {
            root,
            out,
            no_hash,
            rigor,
            evidence,
            cache,
            symlinks_direct,
            junk,
            include,
            exclude,
            progress,
        } => {
            if !root.is_dir() {
                eprintln!("error: not a directory: {}", root.display());
                return Ok(2);
            }
            let ids = match junk::parse_preset_ids(&junk) {
                Ok(ids) => ids,
                Err(message) => {
                    eprintln!("error: {message}");
                    return Ok(2);
                }
            };
            let mut excludes = junk::expand_junk_presets(&ids);
            excludes.extend(exclude.iter().cloned());
            // One ladder, shared with `Job::rigor_resolved`: preset → detail overrides.
            let r = job::rigor::RigorResolved::from_preset(&rigor)
                .with_evidence(evidence.as_deref())
                .with_cache(match cache.as_deref() {
                    Some("on") => Some(true),
                    Some("off") => Some(false),
                    _ => None,
                })
                .with_hash_disabled(no_hash);
            let sopt = scan::ScanOptions {
                hash: r.hash,
                sampled: r.sampled,
                use_cache: r.use_cache,
                symlinks_direct,
                filter: filter::PathFilter::build(&include, &excludes),
            };
            let bar = |p: scan::ScanProgress| {
                let ratio = |done: u64, total: u64| -> u64 {
                    if total == 0 {
                        0
                    } else {
                        ((done.min(total) as u128 * 100) / total as u128) as u64
                    }
                };
                let file_pct = ratio(p.files_done, p.files_total);
                let byte_pct = ratio(p.bytes_done, p.bytes_total);
                let work_pct = match (p.files_total > 0, p.bytes_total > 0) {
                    (true, true) => file_pct.min(byte_pct),
                    (true, false) => file_pct,
                    (false, true) => byte_pct,
                    (false, false) => 0,
                };
                let pct = if p.complete { 100 } else { work_pct.min(99) };
                let files = if p.phase == "walk" && !p.complete && p.files_total == 0 {
                    format!("{}/? files", p.files_done)
                } else {
                    format!("{}/{} files", p.files_done, p.files_total)
                };
                eprint!(
                    "\r{} {:>3}%  {}  {}/{}  {:.1} MiB/s   ",
                    p.phase,
                    pct,
                    files,
                    syncdash::foundation::fmt::human_bytes(p.bytes_done),
                    syncdash::foundation::fmt::human_bytes(p.bytes_total),
                    p.mib_per_s
                );
            };
            let snap = if progress {
                let r = scan::scan_with_progress(&root, &sopt, Some(&bar))?;
                eprintln!();
                r
            } else {
                scan::scan(&root, &sopt)?
            };
            eprintln!(
                "scanned {} entries in {} ms ({})",
                snap.header.entry_count, snap.header.duration_ms, snap.header.root
            );
            write_out(&out, |w| snap.write_to(w))?;
            Ok(0)
        }
        _ => unreachable!("scan handler received another command"),
    }
}
