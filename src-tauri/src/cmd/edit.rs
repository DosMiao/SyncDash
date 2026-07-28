//! What the job editor needs while a job is being edited: whether the roots look right, what a
//! filter mask would hit, and the junk presets available to tick.


use crate::dto::{JunkPresetDto, PathInfo, PathVerdict};

/// Normalize to a comparable form: lowercase + '/' separators + no trailing separator.
/// Used only to decide "are the two roots the same / nested"; it never affects sync semantics.
fn norm_root(p: &str) -> String {
    let s = p.trim().replace('\\', "/").to_lowercase();
    let s = s.trim_end_matches('/');
    s.to_string()
}
/// Live health check for the editor: whether the path exists, whether it is a directory, whether it
/// carries a mount-point marker, and how the two roots relate (identical / nested). A mistyped path costs
/// too much to be reported only in the status bar once Compare runs.
#[tauri::command]
pub fn inspect_paths(source: String, target: String) -> PathVerdict {
    fn info(p: &str) -> PathInfo {
        if p.trim().is_empty() {
            return PathInfo::default();
        }
        let path = std::path::Path::new(p.trim());
        let is_dir = path.is_dir();
        PathInfo {
            exists: is_dir || path.is_file(),
            is_dir,
            has_marker: is_dir && syncdash::pipeline::guard::marker::has_marker(path),
        }
    }
    // A remote phrase is not a local path: filesystem probes would cry wolf. Say what
    // it is instead; connectivity is Compare's job (or `syncdash caps "<phrase>"`).
    let phrase_note = |raw: &str| -> Option<String> {
        use syncdash::fs::vfs::spec::{parse, RootSpec};
        match parse(raw) {
            RootSpec::Remote(r) => Some(format!(
                "{}:// root — checked live at Compare{}",
                r.scheme,
                if r.scheme == "smb" { "; recommend require_marker = true (an unmounted share must never look like an empty directory)" } else { "" }
            )),
            RootSpec::UnknownScheme { scheme, .. } => {
                Some(format!("UNKNOWN scheme '{scheme}://' — this will be refused, never treated as a local path"))
            }
            RootSpec::Local(_) => None,
        }
    };
    let mut v = PathVerdict { source: info(&source), target: info(&target), warnings: Vec::new() };
    let (s, t) = (source.trim(), target.trim());
    let (s_note, t_note) = (phrase_note(s), phrase_note(t));
    if let Some(n) = &s_note {
        v.source.exists = true;
        v.source.is_dir = true;
        v.warnings.push(format!("source: {n}"));
    }
    if let Some(n) = &t_note {
        v.target.exists = true;
        v.target.is_dir = true;
        v.warnings.push(format!("target: {n}"));
    }
    if s_note.is_none() {
        if !s.is_empty() && !v.source.exists {
            v.warnings.push(format!("source does not exist: {s}"));
        } else if !s.is_empty() && !v.source.is_dir {
            v.warnings.push("source is not a directory".into());
        }
    }
    if t_note.is_none() {
        if !t.is_empty() && !v.target.exists {
            v.warnings.push(format!("target does not exist: {t} (it will be created on the first sync)"));
        } else if !t.is_empty() && !v.target.is_dir {
            v.warnings.push("target is not a directory".into());
        }
    }
    let (ns, nt) = (norm_root(s), norm_root(t));
    if !ns.is_empty() && ns == nt {
        v.warnings.push("source and target are the same directory".into());
    } else if !ns.is_empty() && !nt.is_empty() {
        // Nesting: mirror pours the outer contents into the inner root, then treats what it poured in as new outer content — it eats its own tail
        if nt.starts_with(&format!("{ns}/")) {
            v.warnings.push("target sits under source — nested roots copy into themselves".into());
        } else if ns.starts_with(&format!("{nt}/")) {
            v.warnings.push("source sits under target — nested roots copy into themselves".into());
        }
    }
    v
}
/// Ad-hoc mask matching for the UI funnel. The frontend **does not write its own glob** — the FFS mask
/// semantics have exactly one implementation, in filter.rs, so a mask tried out in the UI behaves identically once written into the job's exclude.
#[tauri::command]
pub fn mask_match(masks: Vec<String>, paths: Vec<String>) -> Vec<bool> {
    syncdash::pipeline::filter::mask_hits(&masks, &paths)
}
/// The junk presets, patterns and all. The frontend **does not carry its own copy of these lists** —
/// the editor's checkboxes write literally what the engine would have applied, which is the whole
/// reason a checkbox can now be trusted to describe what a job excludes.
#[tauri::command]
pub fn junk_presets() -> Vec<JunkPresetDto> {
    syncdash::job::junk::JUNK_PRESETS
        .iter()
        .map(|p| JunkPresetDto {
            id: p.id.to_string(),
            label: p.label.to_string(),
            hint: p.hint.to_string(),
            patterns: p.patterns.iter().map(|s| s.to_string()).collect(),
            default_on: p.default_on,
        })
        .collect()
}
