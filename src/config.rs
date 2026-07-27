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
    /// 严谨级：quick（不 hash，size+mtime）| fast（≥4MB 抽样摘要：头/中/尾各 256KB——比 quick 多内容防线，
    /// 比 standard 少读约百倍；云盘/媒体库推荐）| standard（全量 hash＋缓存，默认）| paranoid（全量重 hash＋复制后校验）
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

    // ---------- v0.9 安全网与新能力 ----------
    /// 要求两侧 root 都有 `.syncdash-root` 标记才动手（防 SMB 共享盘没挂上就把对面当空目录）。
    /// 新任务建议开；`syncdash mark <root>` 打标记。
    #[serde(default)]
    pub require_marker: bool,
    /// 至少保留的空闲磁盘比例（0.01 = 1%）。0 关闭
    #[serde(default = "default_min_free")]
    pub min_free_pct: f64,
    /// 单侧删除条目占比超过它就拒绝执行（0.5 = 50%）。0 或 >=1 关闭。
    /// 过滤器写错、source/target 写反、共享盘没挂上，长得都跟这个一模一样。
    #[serde(default = "default_max_delete_ratio")]
    pub max_delete_ratio: f64,
    /// 临时文件 rename 前 fsync。默认开；SMB 上嫌慢可关（自担风险）
    #[serde(default = "default_true")]
    pub fsync: bool,
    /// 冲突策略：report（默认，只报告）| copy（败方存成 .sync-conflict 副本）| newer（新者直接覆盖）
    #[serde(default = "default_conflict")]
    pub on_conflict: String,
    /// on_conflict = "copy" 时每个文件最多保留几份冲突副本（-1 = 不限）
    #[serde(default = "default_max_conflicts")]
    pub max_conflicts: i32,
    /// 同步 unix 权限位（两侧都是 unix 才有意义）
    #[serde(default)]
    pub sync_mode: bool,
    /// 这些路径不参与同步，但删除父目录时可连带删掉（syncthing 的 `(?d)`）
    #[serde(default)]
    pub deletable: Vec<String>,
    /// 本地/挂载盘的增量更新：多读一遍目标换少写很多字节。
    /// SMB/WAN 上传是净赚，对称链路打平——所以默认关，按链路自行开。
    #[serde(default)]
    pub delta: bool,
    /// 系统垃圾排除预设：auto（Win+Mac 两份都上，默认——跨机同步两边都要防）| windows | mac | off。
    /// 排除量永远显示在界面"⚠ 已排除"里，绝不静默
    #[serde(default = "default_os_excludes")]
    pub os_excludes: String,
    /// 排除可重建开发产物（.git/node_modules/target/build/dist/venv…）。
    /// **默认关——.git 也是正常文件**；代码同步任务显式打开（gen-jobs 生成的 cs-* 已带）
    #[serde(default)]
    pub dev_excludes: bool,
    /// Copy/Update 相位的并行宽度（1 = 顺序）。缺省 4；clamp 1..=16
    #[serde(default)]
    pub parallel: Option<usize>,
    /// M6 定时扫描：每 N 秒自动比对（None = 关闭）。秒级档＝"准实时"；UNC 目标建议 ≥30
    #[serde(default)]
    pub watch_interval_secs: Option<u64>,
    /// watch 发现差异时自动执行（默认 false = 只提醒不动手）
    #[serde(default)]
    pub watch_auto_apply: bool,
}

impl Default for Job {
    fn default() -> Self {
        Job {
            mode: "mirror".into(),
            source: PathBuf::new(),
            target: PathBuf::new(),
            archive: None,
            include: Vec::new(),
            exclude: Vec::new(),
            no_hash: false,
            rigor: default_rigor(),
            case_sensitive: false,
            symlinks: default_symlinks(),
            versioning: false,
            remote_host: None,
            remote_root: None,
            remote_exe: None,
            require_marker: false,
            min_free_pct: default_min_free(),
            max_delete_ratio: default_max_delete_ratio(),
            fsync: true,
            on_conflict: default_conflict(),
            max_conflicts: default_max_conflicts(),
            sync_mode: false,
            deletable: Vec::new(),
            delta: false,
            os_excludes: default_os_excludes(),
            dev_excludes: false,
            parallel: None,
            watch_interval_secs: None,
            watch_auto_apply: false,
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_os_excludes() -> String {
    "auto".into()
}
fn default_min_free() -> f64 {
    0.01
}
fn default_max_delete_ratio() -> f64 {
    0.5
}
fn default_conflict() -> String {
    "report".into()
}
fn default_max_conflicts() -> i32 {
    5
}

impl Job {
    pub fn guards(&self, acknowledged: bool) -> crate::preflight::Guards {
        crate::preflight::Guards {
            require_marker: self.require_marker,
            min_free_pct: self.min_free_pct,
            max_delete_ratio: self.max_delete_ratio,
            acknowledged,
        }
    }

    pub fn compare_opts(&self) -> crate::compare::CompareOptions {
        crate::compare::CompareOptions {
            case_insensitive: !self.case_sensitive,
            conflict: match self.on_conflict.as_str() {
                "copy" => crate::compare::ConflictPolicy::Copy,
                "newer" => crate::compare::ConflictPolicy::Newer,
                _ => crate::compare::ConflictPolicy::Report,
            },
            sync_mode: self.sync_mode,
            max_conflicts: self.max_conflicts,
        }
    }

    pub fn apply_opts(&self, trash: Option<PathBuf>, verbose: bool) -> crate::apply::ApplyOptions {
        crate::apply::ApplyOptions {
            dry_run: false,
            trash,
            verbose,
            // 写后校验属于 standard 及以上：T4（传输损坏）是最吓人的静默故障，
            // 校验成本上界 = 本轮传输量，日常档花得起
            verify: matches!(self.rigor.as_str(), "standard" | "paranoid"),
            versioning: self.versioning,
            fsync: self.fsync,
            filter: Some(crate::filter::PathFilter::build_full_opt(&self.include, &self.exclude, &self.deletable, &self.os_excludes, self.dev_excludes)),
            delta: self.delta,
            parallel: self.parallel.unwrap_or(4).clamp(1, 16),
        }
    }
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
# rigor = "standard"                    # quick | fast（抽样摘要，云盘/媒体库推荐）| standard | paranoid（复制后校验）
# case_sensitive = false                # 默认大小写不敏感（NTFS/APFS 默认行为）
# symlinks = "exclude"                  # exclude | direct（同步链接本身）
# versioning = true                     # 被删/被覆盖文件存进各 root 的 .version_syncDash/
#                                       #（syncdash versions / restore 查看与找回；默认走本机 trash）
# no_hash = false
#
# --- 安全闸门（v0.9）---
# require_marker = true                 # 两侧 root 都要有 .syncdash-root 才动手
#                                       #（`syncdash mark <root>` 打标记；防共享盘没挂上就把对面当空目录）
# min_free_pct = 0.01                   # 写入后至少保留的空闲比例；0 关闭
# max_delete_ratio = 0.5                # 单侧删除占比超过它就拒绝执行（--i-know 可放行）；0 关闭
# fsync = true                          # 临时文件 rename 前 fsync；SMB 嫌慢可关（自担风险）
#
# --- 冲突与权限 ---
# on_conflict = "report"                # report（默认，只报告）| copy（败方存 .sync-conflict 副本）| newer
# max_conflicts = 5                     # on_conflict="copy" 时每个文件保留几份副本（-1 不限）
# sync_mode = false                     # 同步 unix 权限位（两侧都是 unix 才有意义）
#
# os_excludes = "auto"                  # 系统垃圾预设：auto（Win+Mac 都排，默认）| windows | mac | off
# dev_excludes = false                  # 排除开发产物（.git/node_modules/target…）。默认关——.git 也是正常文件；
#                                       # 代码同步任务设 true。排除量永远显示在界面"⚠ 已排除"，绝不静默
#
# --- 过滤器扩展 ---
# exclude = ['*/*.log', '!*/audit.log'] # `!` 前缀 = 例外，压过其它 exclude
# deletable = ['*/node_modules/']       # 不同步，但删父目录时可连带删（syncthing 的 (?d)）
#
# --- 增量与并行 ---
# delta = true                          # 本地/挂载盘大文件按块增量写；SMB 上传划算，对称链路打平
# parallel = 4                          # Copy/Update 并行宽度（1 = 顺序；SMB 上 2-4 条流基本吃满上行）
#
# --- 值守（M6 定时扫描）---
# watch_interval_secs = 30              # 每 N 秒自动比对；hash 缓存让未变的树只付 walk 成本
# watch_auto_apply = false              # 发现差异自动执行（默认只提醒）
#
# 远程管线（可选）：远端在自己盘上扫描（快），target 侧打包经 ssh 送达执行
# remote_host = 'mac'
# remote_root = '/Users/xxx/Code/some/dir'
# remote_exe = '~/Code/Utilities/SyncDash/target/release/syncdash'
"#;
