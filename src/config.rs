//! 任务配置：一个 TOML 一个任务（参考 FFS "一个 .ffs_gui 一个配置"的形态）。
//! 位置：Windows %APPDATA%\syncdash\jobs\*.toml，mac ~/.config/syncdash/jobs/*.toml

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Job {
    /// mirror | sync | enrich
    pub mode: String,
    pub source: PathBuf,
    pub target: PathBuf,
    /// sync 模式的上次同步存档；apply 成功后自动刷新
    #[serde(default)]
    pub archive: Option<PathBuf>,
    /// 追加排除的目录/文件名
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub no_hash: bool,
}

pub fn jobs_dir() -> PathBuf {
    if let Ok(a) = std::env::var("APPDATA") {
        PathBuf::from(a).join("syncdash").join("jobs")
    } else if let Ok(h) = std::env::var("HOME") {
        PathBuf::from(h).join(".config").join("syncdash").join("jobs")
    } else {
        PathBuf::from("syncdash-jobs")
    }
}

pub fn load(name_or_path: &str) -> std::io::Result<(String, Job)> {
    let p = PathBuf::from(name_or_path);
    let path = if p.is_file() {
        p
    } else {
        let cand = jobs_dir().join(format!("{name_or_path}.toml"));
        if !cand.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("job not found: {name_or_path} (looked at {})", cand.display()),
            ));
        }
        cand
    };
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let text = std::fs::read_to_string(&path)?;
    let job: Job = toml::from_str(&text)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad job file {}: {e}", path.display())))?;
    Ok((name, job))
}

pub fn load_all() -> Vec<(String, Job)> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(jobs_dir()) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().map(|x| x == "toml").unwrap_or(false) {
                if let Ok(pair) = load(&p.to_string_lossy()) {
                    out.push(pair);
                }
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

pub const SAMPLE: &str = r#"# %APPDATA%\syncdash\jobs\<名字>.toml —— 一个文件一个任务
mode = "mirror"            # mirror | sync | enrich
source = 'D:\some\dir'
target = '\\host\share\dir'
# archive = 'C:\Users\me\AppData\Roaming\syncdash\archive\<名字>.jsonl'   # sync 模式用
# exclude = ["big_temp"]
# no_hash = false
"#;
