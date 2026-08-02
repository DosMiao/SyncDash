//! Which side of an operation a Windows naming fault actually endangers.
//!
//! The gate runs twice by design — compare refuses a bad name at plan time so the user sees it as
//! a conflict, and apply refuses it again at execution time so a hand-edited or stale plan cannot
//! smuggle one through. Running it twice is correct. *Maintaining* it twice is not: the two copies
//! computed the same three facts from the same inputs, and a name fault decides whether an
//! operation can address or delete the wrong file, so a drift between them is a silent
//! data-safety divergence.
//!
//! What is shared here is the decision. What is deliberately **not** shared is the wording: compare
//! renders "(executing side)" into a conflict reason a user reads in the plan, apply renders
//! "executing and reading" into a refusal string. Those are two different surfaces with two
//! different audiences, and folding them together would change messages for no benefit.

use crate::foundation::names::WindowsNameFault;
use crate::fs::vfs::NameRules;
use crate::model::plan::Action;

/// What an operation does with names, which decides whether a fault can reach it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NameRoles {
    /// The operation writes a name that does not exist yet, so an unusable name is a real problem
    /// even when it does not mangle addressing.
    pub(crate) creates_name: bool,
    /// The operation reads from the other root, so that root's naming rules apply too.
    pub(crate) reads_other_root: bool,
}

impl NameRoles {
    pub(crate) fn of(action: &Action) -> Self {
        Self {
            // Update already addresses an existing entry; only Copy and Move introduce a name.
            creates_name: matches!(action, Action::Copy | Action::Move),
            reads_other_root: matches!(action, Action::Copy | Action::Update),
        }
    }
}

/// Which roots a fault endangers, under one set of naming rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HazardSides {
    pub(crate) executing: bool,
    pub(crate) reading: bool,
}

impl HazardSides {
    pub(crate) fn any(self) -> bool {
        self.executing || self.reading
    }
}

/// Whether `fault` endangers the executing root, the reading root, or both, when the roots in
/// question follow `against` naming rules.
///
/// Called once with `NameRules::Windows` (a refusal) and, by compare only, once with
/// `NameRules::Unknown` (a note): a protocol that will not reveal the server's OS cannot prove the
/// name is safe, and a warning is the honest answer rather than a refusal or a silent success.
///
/// A fault that merely makes a name hard to manage counts only where the operation *creates* it;
/// a fault that changes which path is addressed counts wherever the name is used, because an
/// operation can then report success against a different object.
pub(crate) fn hazard_sides(
    fault: WindowsNameFault,
    roles: NameRoles,
    executing_rules: NameRules,
    reading_rules: NameRules,
    against: NameRules,
) -> HazardSides {
    HazardSides {
        executing: executing_rules == against
            && (fault.changes_addressed_path() || roles.creates_name),
        reading: roles.reads_other_root
            && reading_rules == against
            && fault.changes_addressed_path(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_copy_and_move_create_a_name_and_only_copy_and_update_read_the_other_root() {
        assert_eq!(
            NameRoles::of(&Action::Copy),
            NameRoles {
                creates_name: true,
                reads_other_root: true
            }
        );
        assert_eq!(
            NameRoles::of(&Action::Move),
            NameRoles {
                creates_name: true,
                reads_other_root: false
            }
        );
        assert_eq!(
            NameRoles::of(&Action::Update),
            NameRoles {
                creates_name: false,
                reads_other_root: true
            }
        );
        assert_eq!(
            NameRoles::of(&Action::Delete),
            NameRoles {
                creates_name: false,
                reads_other_root: false
            }
        );
    }

    #[test]
    fn an_addressing_fault_reaches_a_root_the_operation_only_reads() {
        let sides = hazard_sides(
            WindowsNameFault::Mangled,
            NameRoles::of(&Action::Update),
            NameRules::Posix,
            NameRules::Windows,
            NameRules::Windows,
        );
        assert!(!sides.executing, "the executing root is posix");
        assert!(
            sides.reading,
            "a mangled name can address a different object on the root being read"
        );
    }

    #[test]
    fn a_usability_fault_counts_only_where_the_name_is_created() {
        // Reserved device names remain addressable; they are only a problem to create.
        let creating = hazard_sides(
            WindowsNameFault::Unusable,
            NameRoles::of(&Action::Copy),
            NameRules::Windows,
            NameRules::Windows,
            NameRules::Windows,
        );
        assert!(creating.executing);
        assert!(
            !creating.reading,
            "reading an awkward name is not a hazard on the root it is read from"
        );

        let updating = hazard_sides(
            WindowsNameFault::Unusable,
            NameRoles::of(&Action::Update),
            NameRules::Windows,
            NameRules::Windows,
            NameRules::Windows,
        );
        assert!(!updating.any(), "Update creates no name");
    }

    #[test]
    fn rules_that_do_not_match_the_question_are_never_a_hazard() {
        let sides = hazard_sides(
            WindowsNameFault::Mangled,
            NameRoles::of(&Action::Copy),
            NameRules::Posix,
            NameRules::Posix,
            NameRules::Windows,
        );
        assert!(!sides.any());

        // The same call asked about Unknown rules is what compare uses to raise a note.
        let unknown = hazard_sides(
            WindowsNameFault::Mangled,
            NameRoles::of(&Action::Copy),
            NameRules::Unknown,
            NameRules::Posix,
            NameRules::Unknown,
        );
        assert!(unknown.executing);
    }
}
