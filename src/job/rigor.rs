//! How much evidence a comparison gathers, resolved from a preset plus per-axis overrides.
//!
//! One ladder, composed by two callers. `Job::rigor_resolved` applies the job file's fields; the
//! CLI's `scan` applies its flags. It used to be written out twice, and the copies had drifted:
//! the CLI's omitted `escalate` and `verify_writes` entirely, so `syncdash scan --rigor paranoid`
//! quietly skipped the post-write verification that the same word means everywhere else.

/// The five axes a rigor setting actually controls. Presets are macros over these; nothing reads
/// the preset name after resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RigorResolved {
    pub hash: bool,
    pub sampled: bool,
    pub use_cache: bool,
    pub escalate: bool,
    pub verify_writes: bool,
}

impl RigorResolved {
    /// The named shortcuts. An unknown name resolves to `standard` rather than failing: rigor
    /// arrives from a hand-editable TOML, and a typo must not stop a sync from running — it
    /// gets the middle setting, which is the one that errs toward more evidence, not less.
    pub fn from_preset(rigor: &str) -> RigorResolved {
        let (hash, sampled, use_cache, escalate, verify_writes) = match rigor {
            "quick" => (false, false, false, false, false),
            "fast" => (true, true, true, true, false),
            "balanced" => (true, true, true, true, true),
            "paranoid" => (true, false, false, false, true),
            _ => (true, true, false, true, true), // standard / custom baseline
        };
        RigorResolved {
            hash,
            sampled,
            use_cache,
            escalate,
            verify_writes,
        }
    }

    /// `none` | `full` | anything else = sampled. Overrides whatever the preset chose.
    pub fn with_evidence(mut self, evidence: Option<&str>) -> RigorResolved {
        match evidence {
            Some("none") => {
                self.hash = false;
                self.sampled = false;
            }
            Some("full") => {
                self.hash = true;
                self.sampled = false;
            }
            Some(_) => {
                self.hash = true;
                self.sampled = true;
            }
            None => {}
        }
        self
    }

    pub fn with_cache(mut self, use_cache: Option<bool>) -> RigorResolved {
        if let Some(v) = use_cache {
            self.use_cache = v;
        }
        self
    }

    pub fn with_escalate(mut self, escalate: Option<bool>) -> RigorResolved {
        if let Some(v) = escalate {
            self.escalate = v;
        }
        self
    }

    pub fn with_verify_writes(mut self, verify: Option<bool>) -> RigorResolved {
        if let Some(v) = verify {
            self.verify_writes = v;
        }
        self
    }

    /// Applied last, and one-way: `--no-hash` can turn hashing off but nothing turns it back on.
    pub fn with_no_hash(mut self, no_hash: bool) -> RigorResolved {
        if no_hash {
            self.hash = false;
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_pick_their_documented_axes() {
        let q = RigorResolved::from_preset("quick");
        assert!(
            !q.hash && !q.sampled && !q.use_cache,
            "quick reads no content at all"
        );

        let f = RigorResolved::from_preset("fast");
        assert!(f.hash && f.sampled && f.use_cache && f.escalate);

        let b = RigorResolved::from_preset("balanced");
        assert!(b.hash && b.sampled && b.use_cache && b.escalate && b.verify_writes);

        let p = RigorResolved::from_preset("paranoid");
        assert!(
            p.hash && !p.sampled,
            "paranoid reads whole files, never windows"
        );
        assert!(p.verify_writes, "and re-reads what it wrote");
        assert!(!p.use_cache, "trusting a cache would defeat the point");
    }

    #[test]
    fn an_unknown_preset_lands_on_standard_rather_than_the_weakest() {
        assert_eq!(
            RigorResolved::from_preset("typo"),
            RigorResolved::from_preset("standard")
        );
        let s = RigorResolved::from_preset("standard");
        assert!(s.hash && s.escalate && s.verify_writes);
    }

    #[test]
    fn a_detail_axis_overrides_its_preset() {
        let r = RigorResolved::from_preset("paranoid").with_evidence(Some("none"));
        assert!(
            !r.hash && !r.sampled,
            "evidence=none wins over paranoid's full read"
        );
        assert!(r.verify_writes, "but it does not touch the other axes");

        let r = RigorResolved::from_preset("quick").with_cache(Some(true));
        assert!(r.use_cache);
    }

    /// `--no-hash` is applied after everything else and is one-directional, so no combination of
    /// preset and evidence flag can put hashing back on behind the user's back.
    #[test]
    fn no_hash_is_applied_last_and_only_turns_things_off() {
        let r = RigorResolved::from_preset("paranoid")
            .with_evidence(Some("full"))
            .with_no_hash(true);
        assert!(!r.hash);
        let r = RigorResolved::from_preset("quick").with_no_hash(false);
        assert!(
            !r.hash,
            "no_hash=false does not enable hashing that the preset left off"
        );
    }
}
