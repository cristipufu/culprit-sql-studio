//! Genuine boxed metadata/leaf acceptance. It never attests the Azure-only source factory or native cost.
use super::*;
use crate::read::{integer, invalid_data};
use fixture::temporal::Witness;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RetentionLayout {
    Available,
    UnsupportedSql2016,
}
impl RetentionLayout {
    pub(super) fn accepts(self, major: i64, columns: i64) -> bool {
        match self {
            Self::Available => major >= 14 && columns == 3,
            Self::UnsupportedSql2016 => major == 13 && columns == 0,
        }
    }
}
#[tokio::test]
#[ignore = "requires exact nonce-owned boxed SQL14+ fixture; real temporal current/history metadata, no FTS component and no skipped positive"]
async fn owned_system_versioned_table_retains_native_history_finite_and_infinite_configuration_without_cleanup_claims()
{
    run(RetentionLayout::Available).boxed_local().await;
}
#[tokio::test]
#[ignore = "separate native SQL2016/major13 environment: absent14+ retention columns are required; unsupported environment is a failure, never a passing skip"]
async fn owned_sql2016_temporal_retention_columns_remain_unsupported_without_erasing_native_roles() {
    run(RetentionLayout::UnsupportedSql2016).boxed_local().await;
}

async fn run(layout: RetentionLayout) {
    let owned = OwnedSourceFixture::open("culprit-temporal-fixture").await.unwrap();
    let mut fixture = Fixture::new(&owned);
    let body = AssertUnwindSafe(async {
        fixture.initialize(false).await?;
        fixture.initialize_temporal(layout).await?;
        let original = compare(&mut fixture, layout).await?;
        let denied = read(&mut fixture, ReadMode::Stream, layout, true).await?;
        let names = fixture.temporal_names();
        assert!(denied.is_empty(), "a native VIEW DEFINITION denial hides objects despite their SELECT grants");
        // The independently observed denial is not an empty complete catalog or per-object absence proof.
        // No original values or object identities may be borrowed from the earlier administrator read.
        if layout == RetentionLayout::Available {
            assert_eq!(original[&names[0]].history_retention_period, Some(6));
            assert_eq!(original[&names[0]].history_retention_period_unit, Some(5));
            fixture.infinite_temporal_retention().await?;
            let changed = compare(&mut fixture, layout).await?;
            assert_eq!(changed[&names[0]].history_retention_period_unit, Some(-1));
            assert_eq!(changed[&names[0]].history_retention_period_unit_desc.as_deref(), Some("INFINITE"));
            assert_eq!(
                original[&names[0]].history_retention_period,
                Some(6),
                "retained originals are not rewritten after later DDL"
            );
        }
        Ok::<_, culprit_core::Error>(original)
    })
    .catch_unwind()
    .await;
    let cleanup = AssertUnwindSafe(fixture.cleanup()).catch_unwind().await;
    let original = match body {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(Err(error)) => panic!("owned temporal metadata acceptance: {error:?}"),
        Ok(Ok(original)) => original,
    };
    cleanup
        .expect("owned temporal cleanup returned")
        .expect("only admitted objects were removed after native readers settled");
    let names = fixture.temporal_names();
    let retained = culprit_checks::table_temporal::configuration_fields(&original[&names[0]]).unwrap();
    assert_eq!(retained[0].text, "2", "original configuration remains interpretable after owned objects are removed");
    if layout == RetentionLayout::Available {
        assert_eq!(original[&names[0]].history_retention_period, Some(6));
    }
    eprintln!(
        "temporal_native_metadata_receipt={}",
        serde_json::json!({"layout":format!("{layout:?}"),"fixture_control":fixture.temporal_accounting(),"source_factory_admission":"unchanged AzureDatabase-only; not validated here","native_cost_approved":false,"application_row_reads":0,"configuration_only":true})
    );
}
async fn compare(
    fixture: &mut Fixture<'_>,
    layout: RetentionLayout,
) -> Result<BTreeMap<String, MssqlTableTemporalConfiguration>> {
    let scan = read(fixture, ReadMode::Scan, layout, false).await?;
    let stream = read(fixture, ReadMode::Stream, layout, false).await?;
    assert_eq!(scan, stream, "same shared table source and decoder; no normalized or borrowed fields");
    let [current, history] = fixture.temporal_names();
    let native = fixture.temporal_witness();
    assert_eq!(stream.len(), 4, "both existing ordinary controls and the native temporal pair remain visible");
    assert_eq!(stream[&current].temporal_type, Some(2));
    assert_eq!(stream[&current].temporal_type_desc.as_deref(), Some("SYSTEM_VERSIONED_TEMPORAL_TABLE"));
    assert_eq!(stream[&current].history_table_id, Some(native.history));
    assert_eq!(stream[&history].temporal_type, Some(1));
    assert_eq!(stream[&history].temporal_type_desc.as_deref(), Some("HISTORY_TABLE"));
    assert_eq!(stream[&history].history_table_id_availability, MssqlCatalogAvailability::Available);
    assert!(stream[&history].history_table_id.is_none(), "genuine native NULL is not unsupported or a fabricated ID");
    for name in [current, history] {
        let fields = culprit_checks::table_temporal::configuration_fields(&stream[&name])?;
        assert_eq!(fields.len(), 6);
        assert_eq!(fields[0].availability, MssqlCatalogAvailability::Available);
        assert_eq!(fields[0].text, culprit_core::format::digits(stream[&name].temporal_type.unwrap().to_string()));
        let expected_id = stream[&name]
            .history_table_id
            .map(|id| culprit_core::format::digits(id.to_string()))
            .unwrap_or_else(|| culprit_core::t!("view.table_temporal.native_null"));
        assert_eq!(fields[2].text, expected_id);
    }
    let expected = if layout == RetentionLayout::Available {
        MssqlCatalogAvailability::Available
    } else {
        MssqlCatalogAvailability::Unsupported
    };
    let configuration = &stream[&fixture.temporal_names()[0]];
    assert_eq!(configuration.history_retention_period_availability, expected);
    assert_eq!(configuration.history_retention_period_unit_availability, expected);
    assert_eq!(configuration.history_retention_period_unit_desc_availability, expected);
    assert_eq!(configuration.history_retention_period, native.period);
    assert_eq!(configuration.history_retention_period_unit, native.unit);
    let interpreted = culprit_checks::table_temporal::configuration_fields(configuration)?;
    if layout == RetentionLayout::Available {
        assert_eq!(interpreted[3].text, culprit_core::format::digits(native.period.unwrap().to_string()));
        assert_eq!(interpreted[4].text, culprit_core::format::digits(native.unit.unwrap().to_string()));
        assert_eq!(interpreted[5].text, configuration.history_retention_period_unit_desc.as_deref().unwrap());
    } else {
        assert!(interpreted[3..].iter().all(|field| field.text == culprit_core::t!("view.table_temporal.unsupported")));
    }
    Ok(stream)
}
async fn one(reader: &mut ReadSession, sql: &str) -> Result<crate::value::Rec> {
    let rows = reader.query(sql, vec![], 2).await.map_err(ReadError::into_error)?;
    let [row] = rows.as_slice() else { return Err(invalid_data()) };
    Ok(row.clone())
}
async fn read(
    fixture: &mut Fixture<'_>,
    mode: ReadMode,
    layout: RetentionLayout,
    denied: bool,
) -> Result<BTreeMap<String, MssqlTableTemporalConfiguration>> {
    assert!(!denied || matches!(mode, ReadMode::Stream));
    let expected_queries = mode.queries() + 2 + u32::from(denied);
    let pool = Arc::new(Pool::new(fixture.config(), None));
    let mut reader = ReadSession::new(
        pool.clone(),
        CollectionBudget {
            max_queries: expected_queries,
            statement_timeout_ms: 1000,
            total_timeout_ms: 5000,
            ..Default::default()
        },
    );
    let oracle = fixture.temporal_oracle(layout);
    let owner = fixture.temporal_witness().clone();
    let impersonate = fixture.impersonate();
    fixture.begin_table_read();
    let work=AssertUnwindSafe(async {
        let before_row=one(&mut reader,&oracle).await?;let before=Witness::decode(&before_row,layout)?;assert!(owner.same_owner(&before),"same original native database/process/table generations");assert!(canonical_time(&owner.at)?<=canonical_time(&before.at)?);
        let session=integer(&before_row,"session_id")?;
        if denied {
            let frame=one(&mut reader,&format!("{impersonate}; SELECT /* culprit fixture denied frame */ DB_ID() AS database_id,@@SPID AS session_id,HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DEFINITION') AS metadata_visible")).await?;
            assert_eq!(integer(&frame,"metadata_visible")?,0,"a genuine native permission negative is required");assert_eq!(integer(&frame,"database_id")?,i64::from(owner.database));assert_eq!(integer(&frame,"session_id")?,session);
        }
        let before_source=reader.queries();
        let values=match mode {
            ReadMode::Scan=>{
                let tables=application(&mut reader,true).await?;assert!(tables.complete_scope);
                tables.tables.into_iter().filter(|table|fixture.owns(&table.schema,&table.name)).map(|table|(table.name,table.mssql_temporal_configuration.expect("actual same-query scan projection"))).collect::<Vec<_>>()
            },
            ReadMode::Stream=>{
                let mut values=Vec::new();
                for (_,table) in stream_table_rows(&mut reader,&Layout::new(!denied)).await? {
                    if fixture.owns(&table.schema,&table.name){fixture.assert_identity(&table);values.push((table.name,table.temporal_configuration));}
                }
                values
            },
        };
        assert_eq!(reader.queries()-before_source,mode.queries(),"original source dispatch count, separately from paid fixture verification");
        let after_sql=if denied{format!("REVERT; {oracle}")}else{oracle.clone()};let after_row=one(&mut reader,&after_sql).await?;let after=Witness::decode(&after_row,layout)?;
        assert!(before.same_owner(&after));assert_eq!(integer(&after_row,"session_id")?,session,"no reconnect can replace the held reader");assert!(canonical_time(&before.at)?<=canonical_time(&after.at)?);
        let mut result=BTreeMap::new();for (name,value) in values {assert!(result.insert(name,value).is_none(),"one original row per table");}
        if denied {
            assert!(result.is_empty(),"metadata-hidden objects cannot be supplied by the earlier administrator read");
        } else {
            assert_eq!(result.len(),4,"the complete original population includes both ordinary and temporal controls");
        }
        eprintln!("temporal_native_read={}",serde_json::json!({"mode":if matches!(mode,ReadMode::Scan){"scan"}else{"stream"},"denied":denied,"native_from":before.at,"native_until":after.at,"reader_queries":reader.queries(),"reader_query_cap":expected_queries,"source_dispatches":mode.queries(),"fixture_verification_dispatches":2+u32::from(denied),"set_setup_reservation":1,"native_rows":result.len(),"native_cost_approved":false}));
        Ok::<_,culprit_core::Error>(result)
    }).catch_unwind().await;
    let shutdown = reader.close().await;
    let metrics = reader.read_metrics();
    let queries = reader.queries();
    drop(reader);
    drop(pool);
    let settled = shutdown == SourceShutdown::Quiescent
        && metrics.as_ref().is_some_and(|metrics| metrics.active_tasks == 0 && !metrics.has_unconfirmed_work());
    if settled {
        fixture.table_read_settled();
    }
    eprintln!(
        "temporal_native_retirement={}",
        serde_json::json!({"reader_queries":queries,"reader_query_cap":expected_queries,"same_owner_quiescent":settled,"active_tasks":metrics.as_ref().map(|metrics|metrics.active_tasks),"unconfirmed_work":metrics.as_ref().map(|metrics|metrics.has_unconfirmed_work()),"further_fixture_sql_withheld":!settled})
    );
    // Unconfirmed work retains the existing fixture debt; cleanup refuses further SQL and the supervisor owns destruction.
    let result = match work {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(Err(error)) => return Err(error),
        Ok(Ok(result)) => result,
    };
    assert!(settled, "the same native reader must settle before assertions or cleanup");
    assert_eq!(queries, expected_queries);
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_version_and_layout_cases_cannot_be_conflated_or_skipped() {
        assert!(RetentionLayout::Available.accepts(14, 3));
        assert!(RetentionLayout::Available.accepts(16, 3));
        assert!(!RetentionLayout::Available.accepts(13, 0));
        assert!(!RetentionLayout::Available.accepts(16, 0));
        assert!(RetentionLayout::UnsupportedSql2016.accepts(13, 0));
        assert!(!RetentionLayout::UnsupportedSql2016.accepts(16, 3));
        assert!(!RetentionLayout::UnsupportedSql2016.accepts(13, 3));
        assert!(!RetentionLayout::UnsupportedSql2016.accepts(13, 1));
        assert!(!RetentionLayout::UnsupportedSql2016.accepts(13, 2));
    }
}
