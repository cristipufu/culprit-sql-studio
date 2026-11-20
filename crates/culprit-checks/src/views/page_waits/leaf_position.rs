//! A recorded leaf link establishes position, never physical continuity, a split, a writer or a latch holder.
use culprit_core::telemetry::{MappedPageTarget, PageWaitTargetKind};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LeafPosition {
    End,
    Followed,
}

impl LeafPosition {
    pub(super) fn from_target(target: &MappedPageTarget) -> Option<Self> {
        if !target.is_valid() || target.kind != PageWaitTargetKind::IndexLeaf || target.header.level != Some(0) {
            return None;
        }
        // Native header fields: https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-objects/sys-dm-db-page-info-transact-sql
        // Index levels are key-order linked lists and (0:0) is the end marker, as used by the writer selector.
        // https://www.sqlskills.com/blogs/paul/checkdb-from-every-angle-complete-description-of-all-checkdb-stages/
        // Keep absent, partial and self-referencing links unknown; no additional page is read.
        match (target.header.next_file_id, target.header.next_page_id) {
            (Some(0), Some(0)) => Some(Self::End),
            (Some(file), Some(page))
                if file > 0 && page > 0 && (file != target.header.file_id || page != target.header.page_id) =>
            {
                Some(Self::Followed)
            }
            _ => None,
        }
    }
}
