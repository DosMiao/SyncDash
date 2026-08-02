//! Ordered, one-way migrations from legacy TOML shapes into the current in-memory schema.

use serde::Deserialize;

use crate::job::model::Job;

/// Apply every migration newer than the schema declared by the file. Individual migrations never
/// stamp the version: the codec does that once after the whole ordered sequence succeeds.
pub(super) fn migrate(job: &mut Job, text: &str) -> Result<(), String> {
    if job.schema < 2 {
        migrate_v1_junk_presets(job, text);
    }
    if job.schema < 3 {
        migrate_v2_peer_target(job, text);
    }
    if job.schema < 4 {
        migrate_v3_current_schema(job, text)?;
    }
    Ok(())
}

/// The keys a v1 job file used to express junk exclusion. Read once, on load, and never written.
#[derive(Deserialize)]
struct LegacyJunkKeys {
    /// "auto" (Windows + macOS) | "windows" | "mac" | "off" — absent meant "auto".
    #[serde(default = "legacy_os_excludes_default")]
    os_excludes: String,
    #[serde(default)]
    dev_excludes: bool,
}

fn legacy_os_excludes_default() -> String {
    "auto".into()
}

/// v1 → v2: materialize junk rules in `exclude`, preserving the exact old filter behavior.
fn migrate_v1_junk_presets(job: &mut Job, text: &str) {
    use crate::job::junk::{expand_junk_presets, same_exclude_entry};
    let legacy: LegacyJunkKeys = toml::from_str(text).unwrap_or(LegacyJunkKeys {
        os_excludes: legacy_os_excludes_default(),
        dev_excludes: false,
    });
    let mut ids: Vec<&str> = match legacy.os_excludes.trim() {
        "off" => vec![],
        "windows" => vec!["windows"],
        "mac" => vec!["macos"],
        _ => vec!["windows", "macos"],
    };
    if legacy.dev_excludes {
        ids.push("dev");
    }
    let mut merged = expand_junk_presets(ids);
    merged.retain(|pattern| {
        !job.exclude
            .iter()
            .any(|entry| same_exclude_entry(entry, pattern))
    });
    merged.append(&mut job.exclude);
    job.exclude = merged;
}

/// The keys a v2 job file used to name a peer. Read once, on load, and never written.
#[derive(Deserialize)]
struct LegacyPeerKeys {
    #[serde(default)]
    remote_host: Option<String>,
    #[serde(default)]
    remote_root: Option<String>,
    #[serde(default)]
    remote_exe: Option<String>,
}

/// v2 → v3: move the peer from three flat fields into the target phrase, preserving its pull mount.
fn migrate_v2_peer_target(job: &mut Job, text: &str) {
    if !job.targets.is_empty() {
        return;
    }
    let Ok(legacy) = toml::from_str::<LegacyPeerKeys>(text) else {
        return;
    };
    let Some(host) = legacy.remote_host.filter(|host| !host.trim().is_empty()) else {
        return;
    };
    let mut phrase = format!(
        "peer://{}/{}",
        host.trim(),
        legacy
            .remote_root
            .unwrap_or_default()
            .trim()
            .replace('\\', "/")
            .trim_matches('/')
    );
    if let Some(exe) = legacy.remote_exe.filter(|exe| !exe.trim().is_empty()) {
        phrase.push_str(&format!("|exe={}", exe.trim()));
    }
    let Ok(mut legacy) = toml::from_str::<LegacyV3Fields>(text) else {
        return;
    };
    let legacy_target = legacy.target.take().unwrap_or_default();
    if !legacy_target.trim().is_empty() {
        phrase.push_str(&format!("|mount={}", legacy_target.trim()));
    }
    job.targets = vec![phrase];
}

#[derive(Deserialize)]
struct LegacyV3Fields {
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    watch_interval_secs: Option<u64>,
    #[serde(default)]
    watch_auto_apply: bool,
    #[serde(default)]
    no_hash: bool,
}

/// v3 → v4: remove settings that duplicated current target, AutoScan, and evidence policy.
fn migrate_v3_current_schema(job: &mut Job, text: &str) -> Result<(), String> {
    let legacy: LegacyV3Fields =
        toml::from_str(text).map_err(|error| format!("cannot read legacy settings: {error}"))?;
    if job.targets.is_empty() {
        let target = legacy
            .target
            .ok_or_else(|| "legacy job has neither `target` nor `targets`".to_string())?;
        job.targets.push(target);
    }
    job.autoscan_interval_secs = legacy.watch_interval_secs;
    job.autoscan_auto_apply = legacy.watch_auto_apply;
    if legacy.no_hash {
        let verify_writes = job.rigor_resolved().verify_writes;
        job.rigor = "custom".into();
        job.evidence = Some("none".into());
        job.use_cache = Some(false);
        job.escalate = Some(false);
        job.verify_writes = Some(verify_writes);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::junk::expand_junk_presets;
    use crate::job::SCHEMA;
    use crate::pipeline::filter::PathFilter;

    use crate::job::persistence::codec::file_schema_at;
    use crate::job::persistence::registry::{load, load_registered_path, validate_job_id};

    fn load_text(tag: &str, text: &str) -> Job {
        let path = std::env::temp_dir().join(format!(
            "syncdash-job-{tag}-{}.toml",
            crate::foundation::time::now_ms()
        ));
        std::fs::write(&path, text).unwrap();
        let (_, job) = load(&path.to_string_lossy()).unwrap();
        let _ = std::fs::remove_file(&path);
        job
    }

    const HEAD: &str = "mode = 'mirror'\nsource = 'S'\ntarget = 'T'\n";

    #[test]
    fn v1_migration_is_ordered_idempotent_and_preserves_user_rules() {
        let job = load_text(
            "auto-dev",
            &format!("{HEAD}os_excludes = 'auto'\ndev_excludes = true\n"),
        );
        assert_eq!(job.schema, SCHEMA);
        assert_eq!(
            job.exclude,
            expand_junk_presets(["windows", "macos", "dev"])
        );
        let filter = PathFilter::build(&job.include, &job.exclude);
        assert!(!filter.pass_file("a/Thumbs.db"));
        assert!(!filter.pass_file("a/.DS_Store"));
        assert!(!filter.pass_dir("proj/.git").0);

        let job = load_text("implicit", HEAD);
        assert_eq!(job.exclude, expand_junk_presets(["windows", "macos"]));

        let job = load_text("off", &format!("{HEAD}os_excludes = 'off'\n"));
        assert!(job.exclude.is_empty());

        let job = load_text("mac", &format!("{HEAD}os_excludes = 'mac'\n"));
        assert_eq!(job.exclude, expand_junk_presets(["macos"]));
        assert!(!PathFilter::build(&[], &job.exclude).pass_file("a/.DS_Store"));
        assert!(PathFilter::build(&[], &job.exclude).pass_file("a/Thumbs.db"));

        let job = load_text(
            "mixed",
            &format!("{HEAD}os_excludes = 'windows'\nexclude = ['*/big_temp/', '*\\Thumbs.db', '!*/keep.log']\n"),
        );
        assert_eq!(
            job.exclude
                .iter()
                .filter(|entry| entry.to_lowercase().contains("thumbs.db"))
                .count(),
            1
        );
        assert!(job.exclude.contains(&"*/big_temp/".to_string()));
        assert!(job.exclude.contains(&"!*/keep.log".to_string()));
        let filter = PathFilter::build(&[], &job.exclude);
        assert!(!filter.pass_file("x/big_temp/f"));
        assert!(filter.pass_file("x/keep.log"));

        let job = load_text(
            "v2",
            &format!("schema = 2\n{HEAD}exclude = ['*/only_this/']\n"),
        );
        assert_eq!(job.exclude, vec!["*/only_this/".to_string()]);
        let job = load_text("v2-empty", &format!("schema = 2\n{HEAD}"));
        assert!(job.exclude.is_empty());

        let job = load_text(
            "v1-peer",
            "mode = 'mirror'\nsource = 'S'\ntarget = 'T'\nos_excludes = 'windows'\n\
             remote_host = 'mac'\nremote_root = '/Users/ben/x'\n",
        );
        assert_eq!(job.schema, SCHEMA);
        assert_eq!(job.exclude, expand_junk_presets(["windows"]));
        assert_eq!(job.targets, vec!["peer://mac/Users/ben/x|mount=T"]);
    }

    #[test]
    fn a_v2_peer_job_migrates_into_one_phrase_without_losing_the_pull_path() {
        let job = load_text(
            "v2-peer",
            "schema = 2\nmode = 'sync'\nsource = 'D:\\Code\\x'\ntarget = '\\\\mac\\share\\x'\n\
             remote_host = 'mac'\nremote_root = '/Users/ben/Code/x'\nremote_exe = '~/bin/syncdash'\n",
        );
        assert_eq!(job.schema, SCHEMA);
        assert_eq!(
            job.targets,
            vec![r"peer://mac/Users/ben/Code/x|exe=~/bin/syncdash|mount=\\mac\share\x".to_string()]
        );
        assert!(crate::fs::vfs::spec::is_peer(&job.targets[0]));
        let crate::fs::vfs::spec::RootSpec::Endpoint(root) =
            crate::fs::vfs::spec::parse(&job.targets[0])
        else {
            panic!("a migrated peer target must parse as an endpoint root")
        };
        assert_eq!(root.host, "mac");
        assert_eq!(root.root, "Users/ben/Code/x");
        assert_eq!(root.opt("exe"), Some("~/bin/syncdash"));
        assert_eq!(root.opt("mount"), Some(r"\\mac\share\x"));

        let bare = load_text(
            "v2-peer-bare",
            "schema = 2\nmode = 'mirror'\nsource = 'D:\\Code\\x'\ntarget = ''\n\
             remote_host = 'mac'\nremote_root = '/Users/ben/x'\n",
        );
        assert_eq!(bare.targets, vec!["peer://mac/Users/ben/x".to_string()]);

        let local = load_text(
            "v2-nopeer",
            "schema = 2\nmode = 'mirror'\nsource = 'S'\ntarget = 'T'\n",
        );
        assert_eq!(local.targets, vec!["T"]);
        assert!(!crate::fs::vfs::spec::is_peer(&local.targets[0]));
    }

    #[test]
    fn v3_storage_and_runtime_policy_migrate_to_current_fields() {
        let scalar = load_text(
            "v3-scalar-target",
            "schema = 3\nmode = 'mirror'\nsource = 'S'\ntarget = 'T'\n",
        );
        assert_eq!(scalar.targets, vec!["T"]);

        let list = load_text(
            "v3-list-targets",
            "schema = 3\nmode = 'mirror'\nsource = 'S'\ntarget = 'obsolete'\n\
             targets = ['A', 'B']\n",
        );
        assert_eq!(list.targets, vec!["A", "B"]);
        let serialized = toml::to_string_pretty(&list).unwrap();
        assert!(serialized.contains("targets = ["));
        assert!(!serialized.lines().any(|line| line.starts_with("target = ")));

        let migrated = load_text(
            "v3-canonical-policy",
            "schema = 3\nmode = 'mirror'\nsource = 'S'\ntarget = 'T'\n\
             rigor = 'paranoid'\nno_hash = true\nwatch_interval_secs = 45\nwatch_auto_apply = true\n",
        );
        assert_eq!(migrated.autoscan_interval_secs, Some(45));
        assert!(migrated.autoscan_auto_apply);
        assert_eq!(migrated.rigor, "custom");
        assert_eq!(migrated.evidence.as_deref(), Some("none"));
        assert_eq!(migrated.use_cache, Some(false));
        assert_eq!(migrated.escalate, Some(false));
        assert_eq!(migrated.verify_writes, Some(true));
        let resolved = migrated.rigor_resolved();
        assert!(!resolved.hash);
        assert!(resolved.verify_writes);

        let serialized = toml::to_string_pretty(&migrated).unwrap();
        for retired in ["no_hash", "watch_interval_secs", "watch_auto_apply"] {
            assert!(!serialized.lines().any(|line| line.starts_with(retired)));
        }
    }

    #[test]
    fn unsupported_current_and_future_shapes_are_refused() {
        let path = std::env::temp_dir().join(format!(
            "syncdash-job-v4-missing-targets-{}.toml",
            crate::foundation::time::now_ms()
        ));
        std::fs::write(&path, "schema = 4\nmode = 'mirror'\nsource = 'S'\n").unwrap();
        let error = load(&path.to_string_lossy()).unwrap_err();
        let _ = std::fs::remove_file(path);
        assert!(error.to_string().contains("requires at least one target"));

        let path = std::env::temp_dir().join(format!(
            "syncdash-job-future-schema-{}.toml",
            crate::foundation::time::now_ms()
        ));
        std::fs::write(
            &path,
            "schema = 5\nmode = 'mirror'\nsource = 'S'\ntargets = ['T']\n",
        )
        .unwrap();
        let error = load(&path.to_string_lossy()).unwrap_err();
        let _ = std::fs::remove_file(path);
        assert!(error.to_string().contains("schema v5 is newer"));
    }

    #[test]
    fn a_job_written_straight_to_toml_round_trips_untouched() {
        assert_eq!(Job::default().schema, SCHEMA);
        let job = Job {
            source: "S".into(),
            targets: vec!["T".into()],
            exclude: vec!["*/only_mine/".into()],
            ..Default::default()
        };
        let path = std::env::temp_dir().join(format!(
            "syncdash-job-rt-{}.toml",
            crate::foundation::time::now_ms()
        ));
        std::fs::write(&path, toml::to_string_pretty(&job).unwrap()).unwrap();
        let (_, loaded) = load(&path.to_string_lossy()).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(loaded.exclude, job.exclude);
        assert_eq!(loaded.schema, SCHEMA);
    }

    #[test]
    fn saving_stamps_the_current_schema() {
        let stale = Job {
            schema: 1,
            exclude: vec!["*/mine/".into()],
            ..Default::default()
        };
        let text = toml::to_string_pretty(&Job {
            schema: SCHEMA,
            ..stale
        })
        .unwrap();
        assert!(text.contains(&format!("schema = {SCHEMA}")));
        let job = load_text(
            "stale",
            &format!("schema = 1\n{HEAD}exclude = ['*/mine/']\n"),
        );
        assert!(job.exclude.len() > 1);
    }

    #[test]
    fn file_schema_reports_the_file_not_the_migrated_job() {
        let write = |tag: &str, text: &str| {
            let path = std::env::temp_dir().join(format!(
                "syncdash-fs-{tag}-{}.toml",
                crate::foundation::time::now_ms()
            ));
            std::fs::write(&path, text).unwrap();
            path
        };

        let path = write("v1", &format!("{HEAD}os_excludes = 'auto'\n"));
        assert_eq!(
            crate::job::persistence::registry::file_schema(&path.to_string_lossy()).unwrap(),
            1
        );
        assert_eq!(load(&path.to_string_lossy()).unwrap().1.schema, SCHEMA);
        let _ = std::fs::remove_file(&path);

        let path = write("v2", &format!("schema = 2\n{HEAD}"));
        assert_eq!(
            crate::job::persistence::registry::file_schema(&path.to_string_lossy()).unwrap(),
            2
        );
        assert_eq!(load(&path.to_string_lossy()).unwrap().1.schema, SCHEMA);
        let _ = std::fs::remove_file(&path);

        let path = write("cur", &format!("schema = {SCHEMA}\n{HEAD}"));
        assert_eq!(
            crate::job::persistence::registry::file_schema(&path.to_string_lossy()).unwrap(),
            SCHEMA
        );
        let _ = std::fs::remove_file(&path);

        assert!(crate::job::persistence::registry::file_schema("no-such-job-exists-here").is_err());
    }

    #[test]
    fn assigning_registered_identity_preserves_legacy_schema_and_comments() {
        let path = std::env::temp_dir().join(format!(
            "syncdash-job-id-migration-{}-{}.toml",
            std::process::id(),
            crate::foundation::time::now_ms()
        ));
        let text = format!("# keep this explanation\n{HEAD}os_excludes = 'off'\n");
        std::fs::write(&path, &text).unwrap();

        let (_, first) = load_registered_path(&path).unwrap();
        let (_, second) = load_registered_path(&path).unwrap();
        let persisted = std::fs::read_to_string(&path).unwrap();
        assert_eq!(first.job_id, second.job_id);
        validate_job_id(&first.job_id).unwrap();
        assert_eq!(file_schema_at(&path).unwrap(), 1);
        assert!(persisted.contains("# keep this explanation"));
        assert_eq!(persisted.matches("job_id =").count(), 1);

        let _ = std::fs::remove_file(path);
    }
}
