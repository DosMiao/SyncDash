//! Compare CSV presentation, rendering, filenames, and reveal authority.

pub(crate) mod execution;
pub(crate) mod filename;
pub(crate) mod presentation;
pub(crate) mod receipt;
pub(crate) mod render;

#[cfg(test)]
mod fixtures {
    use syncdash::model::plan::{Action, Op, Side};

    /// A fully populated plan row. Row selection and CSV rendering are tested separately but must
    /// keep agreeing about what an operation looks like, so they build it from one place.
    pub(super) fn operation(action: Action, side: Side, path: &str) -> Op {
        Op {
            side,
            action,
            path: path.into(),
            from: None,
            size: Some(10),
            mtime_ms: Some(1_700_000_000_000),
            hash: None,
            link: None,
            mode: None,
            reason: "reviewed".into(),
        }
    }
}
