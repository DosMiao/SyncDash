//! Stable, cross-platform names for exported Compare results.

fn safe_filename_component(value: &str) -> String {
    let mut component = String::new();
    for character in value.trim().chars().take(64) {
        if character.is_control()
            || matches!(
                character,
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
            )
        {
            if !component.ends_with('-') {
                component.push('-');
            }
        } else {
            component.push(character);
        }
    }
    component
        .trim_matches(|character| matches!(character, ' ' | '.' | '-'))
        .to_string()
}

pub(crate) fn default_export_filename(
    job_name: &str,
    compare_run_id: u64,
    generated_at_ms: u64,
) -> Result<String, String> {
    let job_component = safe_filename_component(job_name);
    if job_component.is_empty() {
        return Err("The retained Compare result has no usable job name for export".into());
    }
    let generated_at_ms = i64::try_from(generated_at_ms)
        .map_err(|_| "The export timestamp is outside the supported range".to_string())?;
    Ok(format!(
        "SyncDash-{job_component}-compare-{compare_run_id}-{}.csv",
        syncdash::foundation::time::stamp_compact(generated_at_ms)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_filename_is_cross_platform_safe_and_run_specific() {
        let filename =
            default_export_filename(" Photos: 2026 / Primary? ", 42, 1_700_000_000_000).unwrap();
        assert_eq!(
            filename,
            "SyncDash-Photos- 2026 - Primary-compare-42-20231114-221320.csv"
        );
        assert!(!filename.chars().any(|character| matches!(
            character,
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
        )));
    }
}
