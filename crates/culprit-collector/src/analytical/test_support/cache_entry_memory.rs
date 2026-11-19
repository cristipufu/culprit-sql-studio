//! Synthetic, exactly selected native cache objects; no live ownership or cost qualification is claimed.
use super::*;
use culprit_core::{Engine, ErrorCode, source_id};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheEntryMemoryCase {
    Complete,
    Partial,
    UnknownCacheKeys,
    MissingSql,
    Empty,
    UnavailableSource,
    EightStatements,
    SetOptions,
    SameOptionsVariant,
    UnsupportedSetOptions,
    OtherContext,
    MissingPairSql,
    LiteralFragmentation,
}

pub fn cache_entry_memory_evidence_cases() -> Vec<(CacheEntryMemoryCase, CollectionDescription, Vec<TelemetryRecord>)> {
    [
        CacheEntryMemoryCase::Complete,
        CacheEntryMemoryCase::Partial,
        CacheEntryMemoryCase::UnknownCacheKeys,
        CacheEntryMemoryCase::MissingSql,
        CacheEntryMemoryCase::Empty,
        CacheEntryMemoryCase::UnavailableSource,
        CacheEntryMemoryCase::SetOptions,
        CacheEntryMemoryCase::SameOptionsVariant,
        CacheEntryMemoryCase::UnsupportedSetOptions,
        CacheEntryMemoryCase::OtherContext,
        CacheEntryMemoryCase::MissingPairSql,
        CacheEntryMemoryCase::LiteralFragmentation,
    ]
    .into_iter()
    .map(|case| {
        let (source, records) = cache_entry_memory_evidence(case);
        (case, source, records)
    })
    .collect()
}

pub fn cache_entry_memory_evidence(case: CacheEntryMemoryCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut source, grants) = statement_grant_evidence(StatementGrantCase::Complete);
    let TelemetryRecord::Operational(grant) = &grants[0] else { unreachable!() };
    let OperationalFact::MssqlStatementGrants(grant) = &grant.fact else { unreachable!() };
    let context = NativeReadContext::from_snapshot(&source).unwrap();
    let statements = statements(&grant.context, case);
    let entry = MssqlCacheEntryMemory {
        observed_at: "2020-01-01T06:00:02Z".into(),
        plan_handle: grant.context.identity.plan_handle.clone(),
        sql_handle: grant.context.identity.sql_handle.clone(),
        size_bytes: "9007199254740993".into(),
        cache_object_type: "Compiled Plan".into(),
        object_type: "Prepared".into(),
        lookup_count: "40".into(),
        reference_count: "0".into(),
        cache_keys: if case == CacheEntryMemoryCase::UnknownCacheKeys {
            vec![CacheKeyAttribute { name: "set_options".into(), native_type: None, value: None }]
        } else {
            vec![CacheKeyAttribute {
                name: "set_options".into(),
                native_type: Some("int".into()),
                value: Some("1".into()),
            }]
        },
        statements,
    };
    let mut entries = vec![entry];
    if let Some(other) = paired_entry(&entries[0], case) {
        if case == CacheEntryMemoryCase::OtherContext {
            entries[0].cache_keys.push(user_key("1"));
        }
        entries.push(other);
    }
    let focus = CacheEntryMemoryFocus {
        context,
        statements: entries
            .iter()
            .flat_map(|entry| entry.statements.iter().map(|statement| statement.context.clone()))
            .collect(),
    };
    assert!(entries.iter().all(|entry| entry.is_valid() && focus.accepts_entry(entry)));
    let records = if matches!(case, CacheEntryMemoryCase::Empty | CacheEntryMemoryCase::UnavailableSource) {
        vec![]
    } else {
        entries
            .into_iter()
            .map(|entry| {
                TelemetryRecord::Operational(OperationalObservation {
                    source: source_id::CACHE_ENTRY_MEMORY.into(),
                    key: entry.key().unwrap(),
                    fact: OperationalFact::MssqlCacheEntryMemory(Box::new(entry)),
                })
            })
            .collect()
    };
    let coverage = if case == CacheEntryMemoryCase::UnavailableSource {
        StateSourceCoverage::unavailable(source_id::CACHE_ENTRY_MEMORY.into(), ErrorCode::PermissionDenied, None)
    } else {
        StateSourceCoverage::observed(
            source_id::CACHE_ENTRY_MEMORY.into(),
            records.len() as u32,
            case != CacheEntryMemoryCase::Partial,
        )
    };
    source.source = source_id::CACHE_ENTRY_MEMORY.into();
    source.observation = SourceObservation::CacheEntryMemory {
        snapshot: StateSnapshotDescription {
            dataset: SnapshotDataset::CacheEntryMemory,
            observed_from: "2020-01-01T06:00:02Z".into(),
            observed_until: "2020-01-01T06:00:03Z".into(),
            records: records.len() as u32,
            sources: vec![coverage],
        },
        focus: Box::new(focus),
    };
    assert!(source.observation.covers_request() && source.partition_is_consistent());
    (source, records)
}

fn paired_entry(original: &MssqlCacheEntryMemory, case: CacheEntryMemoryCase) -> Option<MssqlCacheEntryMemory> {
    if !matches!(
        case,
        CacheEntryMemoryCase::SetOptions
            | CacheEntryMemoryCase::SameOptionsVariant
            | CacheEntryMemoryCase::UnsupportedSetOptions
            | CacheEntryMemoryCase::OtherContext
            | CacheEntryMemoryCase::MissingPairSql
            | CacheEntryMemoryCase::LiteralFragmentation
    ) {
        return None;
    }
    let mut entry = original.clone();
    entry.plan_handle = "0x0506".into();
    entry.size_bytes = "131072".into();
    entry.cache_keys[0].value = Some(
        match case {
            CacheEntryMemoryCase::SameOptionsVariant => "1",
            CacheEntryMemoryCase::UnsupportedSetOptions => "4099",
            _ => "4097",
        }
        .into(),
    );
    if case == CacheEntryMemoryCase::OtherContext {
        entry.cache_keys.push(user_key("2"));
    }
    if case == CacheEntryMemoryCase::LiteralFragmentation {
        entry.sql_handle = "0x0708".into();
    }
    for statement in &mut entry.statements {
        statement.context.identity.plan_handle = entry.plan_handle.clone();
        statement.context.identity.sql_handle = entry.sql_handle.clone();
        if case == CacheEntryMemoryCase::MissingPairSql {
            statement.statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
        }
    }
    Some(entry)
}

fn user_key(value: &str) -> CacheKeyAttribute {
    CacheKeyAttribute { name: "user_id".into(), native_type: Some("int".into()), value: Some(value.into()) }
}

fn statements(original: &CachedStatementContext, case: CacheEntryMemoryCase) -> Vec<CacheEntryStatement> {
    let count = if case == CacheEntryMemoryCase::EightStatements { MAX_CACHE_ENTRY_STATEMENTS } else { 2 };
    let texts = (0..count)
        .map(|statement| {
            if case == CacheEntryMemoryCase::EightStatements {
                let columns = (0..50).map(|column| format!("[éééééééééé{column}]")).collect::<Vec<_>>().join(",");
                let text = format!("SELECT {columns} FROM dbo.cache_input_{statement} WHERE id=9264");
                assert!(text.encode_utf16().count() <= 1024);
                assert!(text.len() > 1024, "each available statement exceeds the warehouse's UTF-8 preview");
                text
            } else {
                format!("SELECT value FROM dbo.cache_input_{statement} WHERE id=9264")
            }
        })
        .collect::<Vec<_>>();
    let batch_bytes = texts.join("; ").encode_utf16().count() * 2;
    let mut start = 0u32;
    texts
        .into_iter()
        .enumerate()
        .map(|(index, text)| {
            let mut context = original.clone();
            let bytes = (text.encode_utf16().count() * 2) as u32;
            context.identity.statement_start_bytes = start;
            context.identity.statement_end_bytes = if index + 1 == count { -1 } else { (start + bytes) as i32 };
            context.source_text_bytes = Some(batch_bytes.to_string());
            start += bytes + 4;
            let statement = if case == CacheEntryMemoryCase::MissingSql {
                DefinitionPayload::Unavailable {
                    reason: if index == 0 { PayloadUnavailable::Restricted } else { PayloadUnavailable::NotCollected },
                }
            } else {
                DefinitionPayload::sql(&text, Engine::Mssql)
            };
            CacheEntryStatement { context, statement }
        })
        .collect()
}

pub fn assert_cache_entry_memory_capture(
    capture: &crate::analytical::evidence::EvidenceCapture,
    source: &CollectionDescription,
    records: &[TelemetryRecord],
) {
    super::assert_operational_capture(capture, source, records);
    let SourceObservation::CacheEntryMemory { snapshot, focus } = &source.observation else { panic!() };
    assert_eq!(snapshot.dataset, SnapshotDataset::CacheEntryMemory);
    assert!(focus.is_valid());
    assert_eq!(source.observation.range().unwrap(), TelemetryRange::Focused { key: focus.key().unwrap() });
    for row in capture.records() {
        let TelemetryRecord::Operational(observation) = &row.row().record else { panic!() };
        let OperationalFact::MssqlCacheEntryMemory(entry) = &observation.fact else { panic!() };
        assert!(focus.accepts_entry(entry));
        let definitions = row.row().record.definition_payloads().collect::<Vec<_>>();
        assert_eq!(definitions.len(), entry.statements.len());
        for (index, definition) in definitions.iter().enumerate() {
            assert_eq!(definition.pointer, format!("/record/data/fact/data/statements/{index}/statement"));
        }
    }
}
