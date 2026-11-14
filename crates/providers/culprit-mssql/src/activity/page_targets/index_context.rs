//! Counter acquisition is bounded by already checked page/catalog targets. Later linkage never erases a point.
use super::{context::LookupContext, mapped_target, rows::HeaderRecord, rows::PageAddress};
use crate::{
    index_operations::{Layout, targeted},
    read::{NativeRead, stops_remaining_reads},
};
use culprit_core::{ErrorCode, Result, telemetry::*};
use std::collections::{BTreeMap, BTreeSet};

pub(super) enum ReadPlan {
    Empty,
    Unavailable { targets: BTreeMap<PageAddress, targeted::Target>, reason: PageIndexOperations },
    Ready { targets: BTreeMap<PageAddress, targeted::Target>, layout: Layout },
}

impl ReadPlan {
    pub(super) async fn prepare<R: NativeRead>(
        reader: &mut R,
        context: &LookupContext,
        headers: &BTreeMap<PageAddress, HeaderRecord>,
    ) -> Result<Self> {
        let targets = headers
            .iter()
            .filter_map(|(address, header)| target(context, header).map(|target| (*address, target)))
            .collect::<BTreeMap<_, _>>();
        if targets.is_empty() {
            return Ok(Self::Empty);
        }
        match Layout::read(reader).await {
            Ok(layout) => Ok(Self::Ready { targets, layout }),
            Err(error) if stops_remaining_reads(&error) => Err(error),
            Err(error) => Ok(Self::Unavailable { targets, reason: unavailable(error.code) }),
        }
    }

    pub(super) async fn acquire<R: NativeRead>(
        self,
        reader: &mut R,
    ) -> Result<BTreeMap<PageAddress, PageIndexOperations>> {
        match self {
            Self::Empty => Ok(BTreeMap::new()),
            Self::Unavailable { targets, reason } => {
                Ok(targets.into_keys().map(|address| (address, reason.clone())).collect())
            }
            Self::Ready { targets, layout } => {
                let selected = targets.values().copied().collect::<BTreeSet<_>>();
                match targeted::read(reader, &layout, &selected).await {
                    Ok(points) => Ok(targets
                        .into_iter()
                        .map(|(address, target)| {
                            let point = match points.get(&target) {
                                None => PageIndexOperations::Missing,
                                Some(point) => PageIndexOperations::Observed {
                                    observed_at: point.observed_at.clone(),
                                    counters: Box::new(point.counters.clone()),
                                    partition_id: point.partition_id.clone(),
                                    object: point.object.clone(),
                                    relation: PageIndexOperationRelation::UnverifiedTarget,
                                },
                            };
                            (address, point)
                        })
                        .collect()),
                    Err(error) if stops_remaining_reads(&error) => Err(error),
                    Err(error) => {
                        let reason = unavailable(error.code);
                        Ok(targets.into_keys().map(|address| (address, reason.clone())).collect())
                    }
                }
            }
        }
    }
}

fn target(context: &LookupContext, record: &HeaderRecord) -> Option<targeted::Target> {
    if record.address.database != context.authority.database_id || !record.address.matches(&record.header).ok()? {
        return None;
    }
    let mapped = mapped_target(context, record).ok()?;
    let exact_layout = match (mapped.kind, record.catalog.index_id, record.catalog.index_type) {
        (PageWaitTargetKind::HeapData, Some(0), Some(0)) => true,
        (PageWaitTargetKind::IndexLeaf | PageWaitTargetKind::IndexBranch, Some(index), Some(1 | 2)) => index > 0,
        _ => false,
    };
    if mapped.object.is_none()
        || !exact_layout
        || record.header.partition_id != record.catalog.partition_id
        || record.catalog.partition_id.as_deref()?.parse::<i64>().ok()? <= 0
    {
        return None;
    }
    let selected = targeted::Target {
        database_id: record.address.database,
        object_id: record.catalog.object_id?.try_into().ok()?,
        index_id: record.catalog.index_id?.try_into().ok()?,
        partition_number: record.catalog.partition_number?,
        partition_id: record.catalog.partition_id.as_deref()?.parse().ok()?,
    };
    selected.is_valid().then_some(selected)
}

pub(super) fn qualify(
    point: &mut PageIndexOperations,
    target: Option<&MappedPageTarget>,
    before: &HeaderRecord,
    after: &HeaderRecord,
) {
    let matches = target.is_some_and(|target| point.matches_target(target));
    let PageIndexOperations::Observed { partition_id, object, relation, .. } = point else { return };
    *relation = if matches {
        PageIndexOperationRelation::CheckedTarget
    } else if !before.same_mapping(after)
        || partition_id.as_ref().is_some_and(|id| before.catalog.partition_id.as_ref() != Some(id))
        || object
            .as_ref()
            .is_some_and(|object| super::object(before).is_some_and(|before| catalog_conflict(&before.native, object)))
    {
        PageIndexOperationRelation::ChangedTarget
    } else {
        PageIndexOperationRelation::UnverifiedTarget
    };
}

fn catalog_conflict(before: &NativeIdentity, current: &NativeIdentity) -> bool {
    before.id != current.id
        || before.qualifiers.iter().any(|(key, value)| current.qualifiers.get(key).is_some_and(|other| other != value))
}

fn unavailable(code: ErrorCode) -> PageIndexOperations {
    match code {
        ErrorCode::PermissionDenied => PageIndexOperations::PermissionDenied,
        _ => PageIndexOperations::Unavailable,
    }
}
