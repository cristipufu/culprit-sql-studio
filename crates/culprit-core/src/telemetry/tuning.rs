//! Native tuning evidence. Query/plan IDs here are unresolved native references within this observation.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct TuningPlanStatistics {
    pub plan_id: Option<String>,
    pub executions: Option<String>,
    pub aborted: Option<String>,
    /// Recommendation-time statistics, not current runtime measurements.
    pub avg_cpu_us: Option<f64>,
    pub stdev_cpu_us: Option<f64>,
}
impl TuningPlanStatistics {
    fn is_valid(&self) -> bool {
        [&self.plan_id, &self.executions, &self.aborted].into_iter().flatten().all(|v| valid_counter(v))
            && [self.avg_cpu_us, self.stdev_cpu_us].into_iter().flatten().all(|v| v.is_finite() && v >= 0.0)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct NativeTuningRecommendation {
    pub name: String,
    pub kind: String,
    pub reason: Option<String>,
    pub state: Option<String>,
    pub state_reason: Option<String>,
    pub score: Option<u8>,
    pub query_id: Option<String>,
    pub regressed: TuningPlanStatistics,
    pub recommended: TuningPlanStatistics,
    pub script: DefinitionPayload,
    pub index: Option<crate::RecommendedIndex>,
    /// Native datetime2 values have no time-zone offset. Preserve them separately from the app's UTC estimate.
    pub valid_since_without_offset: Option<String>,
    pub last_refresh_without_offset: Option<String>,
    pub applied_without_offset: Option<String>,
    pub valid_since_utc_estimate: Option<String>,
    pub applied_utc_estimate: Option<String>,
    /// Query Store exposes this as datetimeoffset, so it is an established UTC instant.
    pub last_execution_at: Option<String>,
    pub is_executable: Option<bool>,
    pub is_revertable: Option<bool>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct NativeForcedPlan {
    pub query_id: String,
    pub plan_id: String,
    pub failures: String,
    pub last_failure_reason: Option<String>,
    pub last_execution_at: Option<String>,
    pub forcing: Option<String>,
    pub reset: crate::ResetMarker,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct NativeQueryHint {
    pub hint_id: String,
    pub query_id: String,
    pub scope: crate::QueryStoreHintScope,
    pub hint: DefinitionPayload,
    pub failures: String,
    pub last_failure_reason: Option<String>,
    pub native_source: Option<i32>,
    pub native_source_name: Option<String>,
    pub reset: crate::ResetMarker,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "kind", content = "data", rename_all = "kebab-case")]
#[ts(export)]
pub enum TuningFact {
    AutomaticOption(crate::AutomaticTuningOption),
    Recommendation(Box<NativeTuningRecommendation>),
    ForcedPlan(NativeForcedPlan),
    QueryHint(NativeQueryHint),
}
impl TuningFact {
    pub fn key(&self) -> String {
        match self {
            Self::AutomaticOption(v) => format!("option:{}", v.name),
            Self::Recommendation(v) => format!("recommendation:{}", v.name),
            Self::ForcedPlan(v) => format!("forced:{}:{}", v.query_id, v.plan_id),
            Self::QueryHint(v) => match &v.scope {
                crate::QueryStoreHintScope::Local => format!("hint:{}:local", v.hint_id),
                crate::QueryStoreHintScope::ReplicaGroup { id } => format!("hint:{}:replica:{id}", v.hint_id),
                crate::QueryStoreHintScope::Unknown => format!("hint:{}:unknown", v.hint_id),
            },
        }
    }
    pub fn redact(&mut self, engine: crate::Engine) {
        let payload = match self {
            Self::Recommendation(v) => &mut v.script,
            Self::QueryHint(v) => &mut v.hint,
            _ => return,
        };
        if let DefinitionPayload::Available { text, .. } = payload {
            *payload = DefinitionPayload::sql(text, engine);
        }
    }
    pub fn is_valid(&self) -> bool {
        let optional_text = |value: &Option<String>| value.as_ref().is_none_or(|v| v.len() <= 16 * 1024);
        let instant =
            |value: &Option<String>| value.as_deref().is_none_or(|v| v.len() <= 64 && canonical_time(v).is_ok());
        let local = |value: &Option<String>| {
            value.as_deref().is_none_or(|v| {
                v.len() <= 64 && chrono::NaiveDateTime::parse_from_str(v, "%Y-%m-%dT%H:%M:%S%.f").is_ok()
            })
        };
        match self {
            Self::AutomaticOption(v) => {
                [&v.name, &v.desired, &v.actual].into_iter().all(|v| valid_key(v)) && optional_text(&v.reason)
            }
            Self::Recommendation(v) => {
                [&v.name, &v.kind].into_iter().all(|v| !v.is_empty() && v.len() <= 16 * 1024)
                    && [&v.reason, &v.state, &v.state_reason].into_iter().all(optional_text)
                    && v.score.is_none_or(|v| v <= 100)
                    && v.query_id.as_deref().is_none_or(valid_counter)
                    && v.regressed.is_valid()
                    && v.recommended.is_valid()
                    && v.script.is_valid()
                    && [&v.valid_since_without_offset, &v.last_refresh_without_offset, &v.applied_without_offset]
                        .into_iter()
                        .all(local)
                    && [&v.valid_since_utc_estimate, &v.applied_utc_estimate, &v.last_execution_at]
                        .into_iter()
                        .all(instant)
                    && v.index.as_ref().is_none_or(|i| {
                        [&i.schema, &i.table].into_iter().all(|v| !v.is_empty() && v.len() <= 2048)
                            && optional_text(&i.name)
                            && i.columns.len() <= 1024
                            && i.include.len() <= 1024
                            && i.columns.iter().chain(&i.include).all(|v| !v.is_empty() && v.len() <= 2048)
                    })
            }
            Self::ForcedPlan(v) => {
                [&v.query_id, &v.plan_id, &v.failures].into_iter().all(|v| valid_counter(v))
                    && optional_text(&v.last_failure_reason)
                    && optional_text(&v.forcing)
                    && instant(&v.last_execution_at)
                    && valid_reset(&v.reset)
            }
            Self::QueryHint(v) => {
                [&v.hint_id, &v.query_id, &v.failures].into_iter().all(|v| valid_counter(v))
                    && match &v.scope {
                        crate::QueryStoreHintScope::ReplicaGroup { id } => valid_counter(id),
                        _ => true,
                    }
                    && v.hint.is_valid()
                    && optional_text(&v.last_failure_reason)
                    && optional_text(&v.native_source_name)
                    && valid_reset(&v.reset)
            }
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct TuningObservation {
    pub source: String,
    pub fact: TuningFact,
}
impl TuningObservation {
    pub fn is_valid(&self) -> bool {
        valid_key(&self.source) && self.fact.is_valid()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn hint() -> TuningFact {
        let text = "OPTION(USE HINT('private-tuning-canary'))".to_string();
        TuningFact::QueryHint(NativeQueryHint {
            hint_id: "1".into(),
            query_id: "3".into(),
            scope: crate::QueryStoreHintScope::ReplicaGroup { id: "1".into() },
            hint: DefinitionPayload::Available {
                sha256: digest(text.as_bytes()),
                text,
                redaction_version: REDACTION_VERSION,
            },
            failures: "9007199254740993".into(),
            last_failure_reason: None,
            native_source: Some(0),
            native_source_name: Some("USER".into()),
            reset: crate::ResetMarker::Unknown,
        })
    }
    fn source() -> CollectionDescription {
        CollectionDescription {
            target: crate::DatabaseIdentity::server(crate::Engine::Mssql, "fixture", "fixture.invalid", 1433, None)
                .into(),
            source: "tuning-state".into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Snapshot(StateSnapshotDescription {
                dataset: SnapshotDataset::Tuning,
                observed_from: "2020-01-01T00:00:00Z".into(),
                observed_until: "2020-01-01T00:00:01Z".into(),
                records: 1,
                sources: vec![StateSourceCoverage::collected(crate::source_id::QUERY_HINTS.into(), 1)],
            }),
        }
    }
    #[test]
    fn export_redacts_native_sql_preserves_exact_counts_and_requires_the_correct_source_profile() {
        let mut source = source();
        let capture = CaptureIdentity { observation_id: "fixture-capture".into(), revision: None };
        let row =
            TelemetryRecord::Tuning(TuningObservation { source: crate::source_id::QUERY_HINTS.into(), fact: hint() });
        let exported = ExportedRecord::new(&source, &capture, row.clone()).unwrap();
        let json = serde_json::to_string(&exported).unwrap();
        assert!(!json.contains("private-tuning-canary"));
        assert!(json.contains("9007199254740993"));
        assert!(exported.record.is_valid());
        let mut second = row.clone();
        let TelemetryRecord::Tuning(TuningObservation { fact: TuningFact::QueryHint(value), .. }) = &mut second else {
            panic!("hint")
        };
        value.scope = crate::QueryStoreHintScope::ReplicaGroup { id: "2".into() };
        assert_ne!(exported.record_id, ExportedRecord::new(&source, &capture, second).unwrap().record_id);
        let SourceObservation::Snapshot(snapshot) = &mut source.observation else { panic!("snapshot") };
        snapshot.dataset = SnapshotDataset::Settings;
        assert!(ExportedRecord::new(&source, &capture, row).is_err());
    }
    #[test]
    fn malformed_counts_and_missing_native_keys_are_rejected() {
        for field in ["failures", "hint_id", "query_id"] {
            let mut fact = hint();
            let TuningFact::QueryHint(value) = &mut fact else { panic!("hint") };
            match field {
                "failures" => value.failures = "-1".into(),
                "hint_id" => value.hint_id.clear(),
                _ => value.query_id = "unknown".into(),
            }
            assert!(!fact.is_valid());
        }
    }
}
