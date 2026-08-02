//! File-level TOML decoding, migration, schema inspection, and staged encoding.

use serde::Deserialize;
use std::path::Path;

use crate::job::model::{Job, SCHEMA};

pub(super) fn file_schema_at(path: &Path) -> std::io::Result<u32> {
    #[derive(Deserialize)]
    struct OnlySchema {
        #[serde(default = "crate::job::model::default_schema")]
        schema: u32,
    }

    let text = std::fs::read_to_string(path)?;
    let parsed: OnlySchema = toml::from_str(&text).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("bad job file {}: {error}", path.display()),
        )
    })?;
    Ok(parsed.schema)
}

pub(super) fn load_path(path: &Path) -> std::io::Result<(String, Job)> {
    let name = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let text = std::fs::read_to_string(path)?;
    let mut job: Job = toml::from_str(&text).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("bad job file {}: {error}", path.display()),
        )
    })?;
    if job.schema > SCHEMA {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "bad job file {}: schema v{} is newer than supported schema v{}",
                path.display(),
                job.schema,
                SCHEMA
            ),
        ));
    }
    crate::job::persistence::migration::migrate(&mut job, &text).map_err(|reason| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("bad job file {}: {reason}", path.display()),
        )
    })?;
    if job.targets.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "bad job file {}: current job schema requires at least one target",
                path.display()
            ),
        ));
    }
    job.schema = SCHEMA;
    Ok((name, job))
}

pub(super) fn staged_text(path: &Path, text: &str) -> std::io::Result<crate::fs::staged::Staged> {
    let mut staged = crate::fs::staged::Staged::create(path)?;
    staged.write_all_from(&mut text.as_bytes())?;
    staged.seal(true)?;
    Ok(staged)
}

pub(super) fn staged_job(path: &Path, job: &Job) -> std::io::Result<crate::fs::staged::Staged> {
    let text = toml::to_string_pretty(job).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("toml serialize: {error}"),
        )
    })?;
    staged_text(path, &text)
}
