//! 路径文本归一化：全仓唯一的比对键实现。
//!
//! 折叠 = NFC ＋（可选）大小写。NFC 不能省：macOS/APFS 默认写 NFD，少了这一步，
//! 同一个名字在 compare 眼里和在 filter 眼里就不是同一个，NFD 路径能从 exclude 掩码
//! 底下溜过去。

use unicode_normalization::UnicodeNormalization;

/// 比对键：NFC 归一化 ＋（可选）大小写折叠。**只用于匹配，不用于 I/O**——
/// 落盘永远用原始字符串，否则会在对端造出一个改了名的文件。
pub fn norm_key(p: &str, case_insensitive: bool) -> String {
    let nfc: String = p.nfc().collect();
    if case_insensitive { nfc.to_uppercase() } else { nfc }
}

/// 大小写不敏感场景的折叠（等价于 `norm_key(p, true)`）。
pub fn fold(p: &str) -> String {
    norm_key(p, true)
}

/// 主机名 → 可安全放进文件名的形式（非字母数字与 `-` 一律换成 `-`）。
pub fn safe_host(host: &str) -> String {
    host.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "é" 的两种拼写：NFC 单码位 U+00E9，NFD 是 "e" + U+0301 组合音标。
    const NFC_E: &str = "caf\u{00e9}.txt";
    const NFD_E: &str = "cafe\u{0301}.txt";

    #[test]
    fn nfd_and_nfc_spellings_fold_together() {
        // 这条就是分歧的核心：没有 NFC 时这两个串不相等，
        // 于是 filter 认不出 compare 认得的同一个文件
        assert_ne!(NFC_E, NFD_E, "前提：两种拼写的字节确实不同");
        assert_eq!(norm_key(NFC_E, false), norm_key(NFD_E, false));
        assert_eq!(norm_key(NFC_E, true), norm_key(NFD_E, true));
    }

    #[test]
    fn case_folding_is_opt_in() {
        assert_eq!(norm_key("A/b.TXT", false), "A/b.TXT");
        assert_eq!(norm_key("A/b.TXT", true), "A/B.TXT");
    }

    #[test]
    fn fold_matches_case_insensitive_norm_key() {
        for s in ["A/b.TXT", NFC_E, NFD_E, "", "混合Case"] {
            assert_eq!(fold(s), norm_key(s, true));
        }
    }

    #[test]
    fn host_becomes_filename_safe() {
        assert_eq!(safe_host("WIN01"), "WIN01");
        assert_eq!(safe_host("my.host:2222"), "my-host-2222");
        assert_eq!(safe_host("主机"), "--");
    }
}
