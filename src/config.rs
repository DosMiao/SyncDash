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
    /// include 白名单（FFS 过滤器语法；留空 = `*` 全部）
    #[serde(default)]
    pub include: Vec<String>,
    /// 追加排除（FFS 过滤器语法，如 `*/big_temp/`、`*/*.log`；默认垃圾/可重建排除已内置）
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub no_hash: bool,
    /// 严谨级：quick（不 hash，size+mtime）| standard（hash＋缓存，默认）| paranoid（全量重 hash＋复制后校验）
    #[serde(default = "default_rigor")]
    pub rigor: String,
    /// 默认 false（大小写不敏感匹配——NTFS/APFS 默认行为）；true 则大小写敏感
    #[serde(default)]
    pub case_sensitive: bool,
    /// symlink 策略：exclude（默认，忽略）| direct（同步链接本身，按指向字符串比对）
    #[serde(default = "default_symlinks")]
    pub symlinks: String,
    /// 版本控制（可选）：true 时被删/被覆盖文件存进各 root 的 .version_syncDash/（历史随数据走），
    /// 配 `syncdash versions` / `syncdash restore` 查看与找回；false 走本机 trash
    #[serde(default)]
    pub versioning: bool,
    /// 远程管线（可选）：设置后 run 走 ssh —— 远端在自己盘上扫描（免 UNC 哈希慢）＋打包送达执行
    #[serde(default)]
    pub remote_host: Option<String>,
    /// 远端根路径（远端机器自己的本地路径，如 /Users/xxx/Code/...）
    #[serde(default)]
    pub remote_root: Option<String>,
    /// 远端 syncdash 可执行文件路径（默认当它在 PATH 里）
    #[serde(default)]
    pub remote_exe: Option<String>,
}

fn default_rigor() -> String {
    "standard".into()
}

fn default_symlinks() -> String {
    "exclude".into()
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

/// 保存任务（GUI 编辑器用）。返回文件路径。
pub fn save_job(name: &str, job: &Job) -> std::io::Result<PathBuf> {
    let dir = jobs_dir();
    std::fs::create_dir_all(&dir)?;
    let text = toml::to_string_pretty(job)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("toml serialize: {e}")))?;
    let path = dir.join(format!("{name}.toml"));
    std::fs::write(&path, text)?;
    Ok(path)
}

pub fn delete_job(name: &str) -> std::io::Result<()> {
    std::fs::remove_file(jobs_dir().join(format!("{name}.toml")))
}

pub const SAMPLE: &str = r#"# %APPDATA%\syncdash\jobs\<名字>.toml —— 一个文件一个任务
mode = "mirror"            # mirror | sync | enrich
source = 'D:\some\dir'
target = '\\host\share\dir'
# archive = 'C:\Users\me\AppData\Roaming\syncdash\archive\<名字>.jsonl'   # sync 模式用
# include = ['*']                       # FFS 过滤器语法白名单（留空 = 全部）
# exclude = ['*/big_temp/', '*/*.log']  # FFS 语法；默认垃圾/可重建排除已内置
# rigor = "standard"                    # quick | standard | paranoid（复制后校验）
# case_sensitive = false                # 默认大小写不敏感（NTFS/APFS 默认行为）
# symlinks = "exclude"                  # exclude | direct（同步链接本身）
# versioning = true                     # 被删/被覆盖文件存进各 root 的 .version_syncDash/
#                                       #（syncdash versions / restore 查看与找回；默认走本机 trash）
# no_hash = false
#
# 远程管线（可选）：远端在自己盘上扫描（快），target 侧打包经 ssh 送达执行
# remote_host = 'mac'
# remote_root = '/Users/xxx/Code/some/dir'
# remote_exe = '~/Code/Utilities/SyncDash/target/release/syncdash'
"#;
