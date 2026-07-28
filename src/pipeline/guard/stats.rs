//! A plan reduced to the per-side totals the space and ratio gates judge.

use crate::model::plan::{Action, Op, Side};

#[derive(Default, Clone, Debug)]
pub struct SideStats {
    /// Bytes that have to be written (copy + update)
    pub write_bytes: u64,
    pub copies: u64,
    pub updates: u64,
    pub deletes: u64,
    pub delete_dirs: u64,
    pub moves: u64,
}
#[derive(Default, Clone, Debug)]
pub struct PlanStats {
    pub source: SideStats,
    pub target: SideStats,
    pub conflicts: u64,
}
pub fn stat_plan(ops: &[Op]) -> PlanStats {
    let mut st = PlanStats::default();
    for op in ops {
        if op.action == Action::Conflict {
            st.conflicts += 1;
            continue;
        }
        let s = match op.side {
            Side::Source => &mut st.source,
            Side::Target => &mut st.target,
        };
        match op.action {
            Action::Copy => {
                s.copies += 1;
                s.write_bytes += op.size.unwrap_or(0);
            }
            Action::Update => {
                s.updates += 1;
                s.write_bytes += op.size.unwrap_or(0);
            }
            Action::Move => s.moves += 1,
            Action::Delete => s.deletes += 1,
            Action::DeleteDir => s.delete_dirs += 1,
            _ => {}
        }
    }
    st
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(side: Side, action: Action, path: &str, size: Option<u64>) -> Op {
        Op {
            side,
            action,
            path: path.into(),
            from: None,
            size,
            mtime_ms: None,
            hash: None,
            link: None,
            mode: None,
            reason: "t".into(),
        }
    }

    #[test]
    fn stats_split_by_side() {
        let ops = vec![
            op(Side::Target, Action::Copy, "a", Some(100)),
            op(Side::Target, Action::Update, "b", Some(50)),
            op(Side::Target, Action::Delete, "c", Some(7)),
            op(Side::Source, Action::Copy, "d", Some(9)),
            op(Side::Target, Action::Conflict, "e", None),
        ];
        let st = stat_plan(&ops);
        assert_eq!(st.target.write_bytes, 150);
        assert_eq!(st.target.deletes, 1);
        assert_eq!(st.source.write_bytes, 9);
        assert_eq!(st.conflicts, 1);
    }
}
