//! Resolves a table's observed naming semantics for comparison planning.

use crate::fs::vfs::NameRules;
use crate::model::table::{ObservedNameRules, TableHeader};

pub(super) fn name_rules_of(header: &TableHeader) -> NameRules {
    match &header.vfs {
        Some(vfs) => match vfs.name_rules {
            ObservedNameRules::Windows => NameRules::Windows,
            ObservedNameRules::Posix => NameRules::Posix,
            ObservedNameRules::Unknown => NameRules::Unknown,
        },
        None if header.os == "windows" => NameRules::Windows,
        None => NameRules::Posix,
    }
}
