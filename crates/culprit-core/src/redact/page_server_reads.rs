//! Shared scan statistics retain their numeric meaning without native lookup authority.
use super::*;
use crate::telemetry::PageServerReadSummary;

pub(super) fn redact(snapshot: &mut Snapshot) -> crate::Result<()> {
    let mut malformed = false;
    if let Some(workload) = &mut snapshot.workload {
        for plan in workload.queries.iter_mut().flat_map(|query| &mut query.plans) {
            let Some(summary) = &mut plan.page_server_reads else { continue };
            if !summary.is_valid() {
                plan.page_server_reads = None;
                malformed = true;
            } else if let PageServerReadSummary::NativeFragment { native, .. } = summary.as_mut() {
                *native = None;
            }
        }
    }
    if malformed { Err(crate::telemetry::CollectionStop::InvalidPage.error()) } else { Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::{NativeIdentity, PageServerReadAvailability, PageServerReadStatistics};

    fn original() -> Snapshot {
        let mut snapshot: Snapshot =
            serde_json::from_str(include_str!("../../../../fixtures/mssql-demo.json")).unwrap();
        let plan = snapshot
            .workload
            .as_mut()
            .unwrap()
            .queries
            .iter_mut()
            .find_map(|query| query.plans.first_mut())
            .expect("a retained plan variant");
        plan.page_server_reads = Some(Box::new(PageServerReadSummary::NativeFragment {
            native: Some(
                NativeIdentity::new("password=PAGE-SERVER-PRIVATE").qualified("path", "/private/PAGE-SERVER-PRIVATE"),
            ),
            executions: "9007199254740993".into(),
            statistics: Box::new(PageServerReadStatistics {
                availability: PageServerReadAvailability::Available,
                last_pages: Some("9007199254740993".into()),
                ..Default::default()
            }),
        }));
        snapshot
    }

    fn first_summary(snapshot: &Snapshot) -> &PageServerReadSummary {
        snapshot
            .workload
            .as_ref()
            .unwrap()
            .queries
            .iter()
            .flat_map(|query| &query.plans)
            .find_map(|plan| plan.page_server_reads.as_deref())
            .unwrap()
    }

    #[test]
    fn sharing_removes_private_lookup_identity_and_keeps_exact_statistics_idempotently() {
        let original = original();
        assert!(first_summary(&original).is_valid());
        let shared = redact_snapshot(&original);
        assert!(!serde_json::to_string(&shared).unwrap().contains("PAGE-SERVER-PRIVATE"));
        assert!(matches!(first_summary(&shared), PageServerReadSummary::NativeFragment { native: None, .. }));
        assert_eq!(first_summary(&shared).statistics().last_pages.as_deref(), Some("9007199254740993"));
        assert_eq!(serde_json::to_value(redact_snapshot(&shared)).unwrap(), serde_json::to_value(&shared).unwrap());
    }

    #[test]
    fn redaction_does_not_repair_invalid_original_statistics_or_identity() {
        for invalid_identity in [true, false] {
            let mut original = original();
            let plan = original
                .workload
                .as_mut()
                .unwrap()
                .queries
                .iter_mut()
                .find_map(|query| query.plans.first_mut())
                .unwrap();
            let PageServerReadSummary::NativeFragment { native, statistics, .. } =
                plan.page_server_reads.as_mut().unwrap().as_mut()
            else {
                unreachable!()
            };
            if invalid_identity {
                native.as_mut().unwrap().id.clear();
            } else {
                statistics.last_pages = Some("-1".into());
            }
            let shared = redact_snapshot(&original);
            assert!(
                shared
                    .workload
                    .as_ref()
                    .unwrap()
                    .queries
                    .iter()
                    .flat_map(|query| &query.plans)
                    .all(|plan| plan.page_server_reads.is_none())
            );
            let outcome = shared.sources.iter().find(|source| source.source == crate::source_id::QUERY_STORE).unwrap();
            assert_eq!(outcome.state, crate::SourceState::Unavailable);
            assert_eq!(outcome.reason.as_deref(), Some(crate::t!("view.page_server_reads.invalid").as_str()));
            assert_eq!(serde_json::to_value(redact_snapshot(&shared)).unwrap(), serde_json::to_value(&shared).unwrap());
        }
    }
}
