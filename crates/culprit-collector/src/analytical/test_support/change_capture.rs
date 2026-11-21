//! One synthetic native context for the two independent database log-capture corpora.
use super::*;
use culprit_core::DatabaseIdentity;
pub(super) fn context(writable: bool) -> NativeReadContext {
    NativeReadContext {
        target: DatabaseIdentity::server(culprit_core::Engine::Mssql, "db", "original.invalid", 1433, None).into(),
        partition: SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: culprit_core::Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("machine"),
                native: NativeIdentity::new("original-instance"),
            }),
            epoch: Some("original-process-and-db".into()),
        },
        observed_from: "2020-01-01T00:00:00Z".into(),
        observed_until: "2020-01-01T00:00:01Z".into(),
        database_writable: Some(writable),
    }
}

/// Controlled repeated native-shaped error rows for presentation/retention controls; never native proof.
pub fn multiple_errors(family: MssqlChangeCaptureFamily, count: u32) -> (CollectionDescription, Vec<TelemetryRecord>) {
    assert!((1..=64).contains(&count));
    let (mut source, rows) = match family {
        MssqlChangeCaptureFamily::Cdc => super::cdc_evidence(super::CdcCase::ErrorAndProgress),
        MssqlChangeCaptureFamily::ChangeFeed => super::change_feed_evidence(super::ChangeFeedCase::ProgressAndError),
    };
    let error_source = family.sources()[2];
    let template = rows
        .iter()
        .find(|record| matches!(record,TelemetryRecord::Operational(row) if row.source==error_source))
        .unwrap()
        .clone();
    let mut rows = rows
        .into_iter()
        .filter(|record| !matches!(record,TelemetryRecord::Operational(row) if row.source==error_source))
        .collect::<Vec<_>>();
    for position in 1..=count {
        let mut row = template.clone();
        let TelemetryRecord::Operational(record) = &mut row else { unreachable!() };
        match &mut record.fact {
            OperationalFact::MssqlCdc(point) => {
                point.position = position;
                record.key = point.key().unwrap();
            }
            OperationalFact::MssqlChangeFeed(point) => {
                point.position = position;
                record.key = point.key().unwrap();
            }
            _ => unreachable!(),
        }
        rows.push(row);
    }
    let SourceObservation::Snapshot(header) = &mut source.observation else { unreachable!() };
    header.records = rows.len() as u32;
    header.sources[2].records = count;
    (source, rows)
}
