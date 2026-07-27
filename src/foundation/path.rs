//! 相对路径的字符串操作。
//!
//! 全仓约定：表里的 rel 一律 `/` 分隔，只有落到真实文件系统时才转本地分隔符。

use std::path::{Path, PathBuf};

/// 表里的 rel（`/` 分隔）→ 本地分隔符。
pub fn to_native(rel: &str) -> String {
    if cfg!(windows) { rel.replace('/', "\\") } else { rel.to_string() }
}

/// root + rel → 真实路径。
pub fn join_native(root: &Path, rel: &str) -> PathBuf {
    root.join(to_native(rel))
}

/// 本地路径 → 表里的 rel。不在 root 下时返回 None。
pub fn to_rel(path: &Path, root: &Path) -> Option<String> {
    let r = path.strip_prefix(root).ok()?;
    Some(r.to_string_lossy().replace('\\', "/"))
}

/// 末段之前的部分，不含结尾的 `/`。顶层返回 None。
pub fn parent(rel: &str) -> Option<&str> {
    rel.rfind('/').map(|i| &rel[..i])
}

/// 末段。
pub fn base_name(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// 拆成 (含尾 `/` 的目录前缀, 末段)。顶层时目录前缀是空串。
pub fn split_parent(rel: &str) -> (&str, &str) {
    match rel.rfind('/') {
        Some(i) => (&rel[..=i], &rel[i + 1..]),
        None => ("", rel),
    }
}

/// 拆成 (主干, 含点的扩展名)。隐藏文件（`.gitignore`）整体算主干，不当扩展名。
pub fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// 从 root 的写法猜本地分隔符。给 CSV 导出/前端拼路径用——那里拿到的是
/// 对端机器的 root 字符串，`cfg!(windows)` 说的是**本机**，会猜反。
pub fn sep_of(root: &str) -> char {
    if root.contains('\\') { '\\' } else { '/' }
}

/// 来路不明的 rel 是否可以安全地落到磁盘上。
///
/// 用于解包等「计划来自对端」的场景：拒绝绝对路径、盘符、`..` 穿越与空段。
pub fn is_safe_rel(rel: &str) -> bool {
    !rel.is_empty()
        && !rel.starts_with('/')
        && !rel.contains(':')
        && !rel.split('/').any(|seg| seg == ".." || seg.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_and_base_split_at_the_last_slash() {
        assert_eq!(parent("a/b/c.txt"), Some("a/b"));
        assert_eq!(parent("top.txt"), None);
        assert_eq!(base_name("a/b/c.txt"), "c.txt");
        assert_eq!(base_name("top.txt"), "top.txt");
        assert_eq!(split_parent("a/b/c.txt"), ("a/b/", "c.txt"));
        assert_eq!(split_parent("top.txt"), ("", "top.txt"));
    }

    #[test]
    fn hidden_files_have_no_extension() {
        assert_eq!(split_ext("report.pdf"), ("report", ".pdf"));
        assert_eq!(split_ext("archive.tar.gz"), ("archive.tar", ".gz"));
        assert_eq!(split_ext(".gitignore"), (".gitignore", ""));
        assert_eq!(split_ext("README"), ("README", ""));
    }

    #[test]
    fn unsafe_rels_are_refused() {
        assert!(is_safe_rel("a/b.txt"));
        assert!(!is_safe_rel(""), "空 rel");
        assert!(!is_safe_rel("/etc/passwd"), "绝对路径");
        assert!(!is_safe_rel("C:/x"), "盘符");
        assert!(!is_safe_rel("a/../../etc"), "穿越");
        assert!(!is_safe_rel("a//b"), "空段");
        assert!(!is_safe_rel(".."));
    }

    #[test]
    fn sep_is_inferred_from_the_root_string_not_the_host() {
        // 这正是 cfg!(windows) 会猜反的场景：本机是 Windows，但对端 root 是 posix
        assert_eq!(sep_of(r"D:\Code\x"), '\\');
        assert_eq!(sep_of("/Users/x/Code"), '/');
    }

    #[test]
    fn rel_roundtrips_through_native() {
        let root = Path::new(if cfg!(windows) { r"D:\R" } else { "/r" });
        let joined = join_native(root, "a/b/c.txt");
        assert_eq!(to_rel(&joined, root).as_deref(), Some("a/b/c.txt"));
    }

    #[test]
    fn to_rel_refuses_paths_outside_root() {
        let root = Path::new(if cfg!(windows) { r"D:\R" } else { "/r" });
        let outside = Path::new(if cfg!(windows) { r"D:\Other\x" } else { "/other/x" });
        assert_eq!(to_rel(outside, root), None);
    }
}
