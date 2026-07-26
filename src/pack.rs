//! v0.4 远端打包（需求 4）：
//!   pack       —— 把计划里 target 侧的操作打成一个 tar 包：
//!                 plan.jsonl（操作清单）+ payload/<rel>（待写入文件）+ manifest.json（收尾）
//!                 manifest 含：计划 blake3、每个 payload 文件的 blake3/size/mtime/unix mode、
//!                 合并 hash（按序连接各文件 hash 再 blake3）
//!   apply-pack —— 对端执行：先验计划 hash → 逐文件提取到 staging 并验 hash →
//!                 复用 apply::apply（锁、回收目录、复制后校验全都在）→ unix 恢复 mode
//! 默认 dry-run；路径安全：拒绝绝对路径与 `..` 分量。

use crate::compare::{Action, Op, Plan, PlanHeader, Side};
use crate::table::now_ms;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone)]
pub struct PayloadEntry {
    pub rel: String,
    pub size: u64,
    pub mtime_ms: i64,
    pub hash: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub mode: Option<u32>,
}

#[derive(Serialize, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub created_ms: u64,
    pub source_host: String,
    pub source_os: String,
    pub target_root_hint: String,
    pub op_count: u64,
    pub plan_blake3: String,
    pub payload: Vec<PayloadEntry>,
    pub payload_combined_blake3: String,
}

fn rel_is_safe(rel: &str) -> bool {
    !rel.is_empty()
        && !rel.starts_with('/')
        && !rel.contains(':')
        && !rel.split('/').any(|seg| seg == ".." || seg.is_empty())
}

fn to_native(rel: &str) -> String {
    if cfg!(windows) { rel.replace('/', "\\") } else { rel.to_string() }
}

struct HashingReader<R: Read> {
    inner: R,
    hasher: blake3::Hasher,
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }
}

fn tar_header(size: u64) -> tar::Header {
    let mut h = tar::Header::new_gnu();
    h.set_size(size);
    h.set_mode(0o644);
    h.set_mtime(now_ms() / 1000);
    h.set_cksum();
    h
}

pub struct PackSummary {
    pub ops: u64,
    pub files: u64,
    pub bytes: u64,
}

/// 打包 plan 中 target 侧的操作。payload 从 source_root 读取。
pub fn pack(plan: &Plan, source_root: &Path, out: &Path) -> std::io::Result<PackSummary> {
    let target_ops: Vec<Op> = plan
        .ops
        .iter()
        .filter(|o| o.side == Side::Target && !matches!(o.action, Action::Conflict | Action::Note))
        .cloned()
        .collect();
    let skipped_source_side = plan.ops.iter().filter(|o| o.side == Side::Source).count();
    if skipped_source_side > 0 {
        eprintln!("note: {skipped_source_side} source-side op(s) not packed (they run locally, not on the remote)");
    }
    for op in &target_ops {
        if !rel_is_safe(&op.path) || op.from.as_deref().map(|f| !rel_is_safe(f)).unwrap_or(false) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("unsafe path in plan: {}", op.path)));
        }
    }

    // 子计划：同 header、只含 target 侧 ops
    let sub = Plan { header: PlanHeader { op_count: target_ops.len() as u64, ..plan.header.clone() }, ops: target_ops.clone() };
    let mut plan_bytes: Vec<u8> = Vec::new();
    sub.write_to(&mut plan_bytes)?;
    let plan_hash = blake3::hash(&plan_bytes).to_hex().to_string();

    let f = std::fs::File::create(out)?;
    let mut tarb = tar::Builder::new(std::io::BufWriter::new(f));
    tarb.append_data(&mut tar_header(plan_bytes.len() as u64), "plan.jsonl", &plan_bytes[..])?;

    // payload：Copy/Update 的内容，去重
    let mut seen = std::collections::HashSet::new();
    let mut payload: Vec<PayloadEntry> = Vec::new();
    let mut bytes_total = 0u64;
    for op in &target_ops {
        if !matches!(op.action, Action::Copy | Action::Update) || !seen.insert(op.path.clone()) {
            continue;
        }
        let src = source_root.join(to_native(&op.path));
        let md = std::fs::metadata(&src)?;
        let size = md.len();
        let mtime_ms = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::MetadataExt;
            Some(md.mode() & 0o7777)
        };
        #[cfg(not(unix))]
        let mode: Option<u32> = None;

        let file = std::fs::File::open(&src)?;
        let mut hr = HashingReader { inner: std::io::BufReader::new(file), hasher: blake3::Hasher::new() };
        tarb.append_data(&mut tar_header(size), format!("payload/{}", op.path), &mut hr)?;
        let hash = hr.hasher.finalize().to_hex().to_string();
        bytes_total += size;
        payload.push(PayloadEntry { rel: op.path.clone(), size, mtime_ms, hash, mode });
    }

    let mut comb = blake3::Hasher::new();
    for p in &payload {
        comb.update(p.hash.as_bytes());
    }
    let manifest = Manifest {
        schema: crate::table::SCHEMA,
        created_ms: now_ms(),
        source_host: crate::table::host_name(),
        source_os: std::env::consts::OS.to_string(),
        target_root_hint: plan.header.target_root.clone(),
        op_count: target_ops.len() as u64,
        plan_blake3: plan_hash,
        payload,
        payload_combined_blake3: comb.finalize().to_hex().to_string(),
    };
    let mani_bytes = serde_json::to_vec_pretty(&manifest)?;
    tarb.append_data(&mut tar_header(mani_bytes.len() as u64), "manifest.json", &mani_bytes[..])?;
    tarb.into_inner()?.flush()?;

    Ok(PackSummary { ops: manifest.op_count, files: manifest.payload.len() as u64, bytes: bytes_total })
}

/// 对端执行包。dry_run=true 只校验+列出。返回 (done, skipped, errors)。
pub fn apply_pack(
    pkg: &Path,
    target_root_override: Option<&Path>,
    do_apply: bool,
    verbose: bool,
) -> std::io::Result<(u64, u64, u64)> {
    // ---------- 第 1 遍：读 plan.jsonl 与 manifest.json ----------
    let mut plan_bytes: Option<Vec<u8>> = None;
    let mut manifest: Option<Manifest> = None;
    {
        let f = std::fs::File::open(pkg)?;
        let mut ar = tar::Archive::new(std::io::BufReader::new(f));
        for entry in ar.entries()? {
            let mut entry = entry?;
            let name = entry.path()?.to_string_lossy().into_owned();
            if name == "plan.jsonl" {
                let mut v = Vec::new();
                entry.read_to_end(&mut v)?;
                plan_bytes = Some(v);
            } else if name == "manifest.json" {
                let mut v = Vec::new();
                entry.read_to_end(&mut v)?;
                manifest = Some(serde_json::from_slice(&v).map_err(|e| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad manifest: {e}"))
                })?);
            }
        }
    }
    let plan_bytes = plan_bytes.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "package has no plan.jsonl"))?;
    let manifest = manifest.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "package has no manifest.json"))?;

    // 计划完整性
    let got = blake3::hash(&plan_bytes).to_hex().to_string();
    if got != manifest.plan_blake3 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("plan hash mismatch: manifest {} vs actual {got}", manifest.plan_blake3)));
    }

    // 解析计划
    let tmp_plan = std::env::temp_dir().join(format!("syncdash-plan-{}.jsonl", std::process::id()));
    std::fs::write(&tmp_plan, &plan_bytes)?;
    let plan = Plan::load(&tmp_plan)?;
    let _ = std::fs::remove_file(&tmp_plan);

    for op in &plan.ops {
        if !rel_is_safe(&op.path) || op.from.as_deref().map(|f| !rel_is_safe(f)).unwrap_or(false) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("unsafe path in package plan: {}", op.path)));
        }
    }

    let target_root: PathBuf = match target_root_override {
        Some(p) => p.to_path_buf(),
        None => PathBuf::from(&plan.header.target_root),
    };
    println!(
        "package: {} op(s), {} payload file(s), from {} ({}) -> target {}",
        plan.ops.len(),
        manifest.payload.len(),
        manifest.source_host,
        manifest.source_os,
        target_root.display()
    );

    if !do_apply {
        for op in &plan.ops {
            println!("DRY  {:?} {}  ({})", op.action, op.path, op.reason);
        }
        println!("dry-run OK: plan hash verified; rerun with --apply");
        return Ok((0, plan.ops.len() as u64, 0));
    }

    if !target_root.is_dir() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("target root not accessible: {}", target_root.display())));
    }

    // ---------- 第 2 遍：提取 payload 到 staging，并逐文件验 hash ----------
    let staging = std::env::temp_dir().join(format!("syncdash-staging-{}-{}", std::process::id(), now_ms()));
    std::fs::create_dir_all(&staging)?;
    let by_rel: std::collections::HashMap<&str, &PayloadEntry> = manifest.payload.iter().map(|p| (p.rel.as_str(), p)).collect();
    let mut extract_errors = 0u64;
    {
        let f = std::fs::File::open(pkg)?;
        let mut ar = tar::Archive::new(std::io::BufReader::new(f));
        for entry in ar.entries()? {
            let mut entry = entry?;
            let name = entry.path()?.to_string_lossy().into_owned();
            let Some(rel) = name.strip_prefix("payload/") else { continue };
            let rel = rel.to_string();
            if !rel_is_safe(&rel) {
                extract_errors += 1;
                eprintln!("ERR  unsafe payload path skipped: {rel}");
                continue;
            }
            let dst = staging.join(to_native(&rel));
            if let Some(par) = dst.parent() {
                std::fs::create_dir_all(par)?;
            }
            let mut out = std::fs::File::create(&dst)?;
            let mut hasher = blake3::Hasher::new();
            let mut buf = [0u8; 1 << 16];
            loop {
                let n = entry.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
                out.write_all(&buf[..n])?;
            }
            let got = hasher.finalize().to_hex().to_string();
            match by_rel.get(rel.as_str()) {
                Some(exp) if exp.hash == got => {
                    if verbose {
                        println!("OK   payload verified: {rel}");
                    }
                }
                Some(exp) => {
                    extract_errors += 1;
                    eprintln!("ERR  payload hash mismatch: {rel} (manifest {} vs {got})", exp.hash);
                    let _ = std::fs::remove_file(&dst);
                }
                None => {
                    extract_errors += 1;
                    eprintln!("ERR  payload not in manifest: {rel}");
                    let _ = std::fs::remove_file(&dst);
                }
            }
        }
    }
    if extract_errors > 0 {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{extract_errors} payload file(s) failed verification — nothing applied")));
    }

    // ---------- 执行：复用 apply（锁、回收目录、复制后校验） ----------
    // 把 op 的 hash/mtime 对齐 manifest（打包时的最新状态），apply 的 verify 用它复检
    let mut ops = plan.ops.clone();
    for op in &mut ops {
        if matches!(op.action, Action::Copy | Action::Update) {
            if let Some(p) = by_rel.get(op.path.as_str()) {
                op.hash = Some(p.hash.clone());
                op.mtime_ms = Some(p.mtime_ms);
            }
        }
    }
    let (done, skipped, errors) = crate::apply::apply(
        &ops,
        &staging,
        &target_root,
        &crate::apply::ApplyOptions { dry_run: false, trash: None, verbose, verify: true },
    );

    // unix：恢复 exec 等权限位（SMB/打包路径上唯一会丢的属性）
    #[cfg(unix)]
    if errors == 0 {
        use std::os::unix::fs::PermissionsExt;
        for op in &ops {
            if matches!(op.action, Action::Copy | Action::Update) {
                if let Some(p) = by_rel.get(op.path.as_str()) {
                    if let Some(mode) = p.mode {
                        let _ = std::fs::set_permissions(target_root.join(to_native(&op.path)), std::fs::Permissions::from_mode(mode));
                    }
                }
            }
        }
    }

    let _ = std::fs::remove_dir_all(&staging);
    Ok((done, skipped, errors))
}
