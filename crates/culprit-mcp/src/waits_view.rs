//! Compact JSON for the `waits` tool: categories with their meaning and top queries, and the pressure signals.

use culprit_core::{FileIo, Report, Signal, WaitSample, format};
use serde_json::{Value, json};

const COMPILATION_ENTRIES: usize = 5;
const COMPILER_ADMISSION_ENTRIES: usize = 5;
const CLEANUP_ENTRIES: usize = 5;
const CLEANUP_BOUNDARY_REFERENCES: usize = 5;
const PARTICIPANT_ENTRIES: usize = 5;
const REPLICATION_ENTRIES: usize = 16;

mod completed_spills;
mod configured_log_rates;
mod connection_outcomes;
mod counter_ratios;
mod jobs;
pub(crate) mod page_server_reads;
mod performance_counters;
mod statement_work;

#[cfg(test)]
mod compiler_admission_tests;
#[cfg(test)]
mod replication_tests;

fn signal(s: &Signal) -> Value {
    json!({
        "key": s.key,
        "label": s.label,
        "value": s.value,
        "unit": s.unit,
        "scope": s.scope,
        "status": s.status,
        "healthy": s.healthy,
    })
}

/// One file's average latency since startup, in words an agent can quote.
fn file(f: &FileIo) -> Value {
    let avg = |ms: Option<f64>, n: f64| ms.filter(|_| n > 0.0).map(|ms| ms / n).map(format::storage_latency);
    json!({
        "file": f.name,
        "kind": f.kind,
        "database": f.database,
        "reads": f.reads,
        "avg_read": avg(f.read_ms, f.reads),
        "writes": f.writes,
        "avg_write": avg(f.write_ms, f.writes),
    })
}

pub fn report(report: &Report) -> Value {
    let snapshot = &report.snapshot;
    let waits = culprit_engine::explain_waits(snapshot);
    let activity = waits.as_ref().and_then(|w| w.activity.as_ref()).map(|a| {
        json!({
            "period":a.period,
            "categories":a.categories.iter().take(12).map(|c|json!({
                "key":c.key,"label":c.label,"share_of_sample_weight":c.share,
                "observations":c.observations,"weighted_observations":c.weighted_observations,
                "events":c.events.iter().take(8).collect::<Vec<_>>(),
                "events_retained":c.events.len(),"queries":c.queries.iter().take(5).collect::<Vec<_>>()
            })).collect::<Vec<_>>()
        })
    });
    let categories: Vec<Value> = waits
        .as_ref()
        .map(|w| {
            w.categories
                .iter()
                .map(|c| {
                    json!({
                        "category": c.label,
                        "key": c.key,
                        "share": format::percent(c.share),
                        "time": format::duration(c.wait_ms),
                        "top_wait": c.top_wait,
                        "meaning": c.meaning,
                        "detail": c.detail,
                        "queries": c.queries.iter().take(5).map(|q| json!({
                            "id": q.id,
                            "recorded_ms": q.wait_ms,
                            "mean_recorded_ms": q.mean_recorded_ms,
                            "text": q.text.chars().take(140).collect::<String>(),
                        })).collect::<Vec<_>>(),
                        "not_attributed": c.attribution,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let mut response = json!({
        "connection": snapshot.connection,
        "source": waits.as_ref().map(|w| w.source.clone()),
        "categories": categories,
        "notes": waits.map(|w| w.notes).unwrap_or_else(|| {
            let mut notes=vec!["The latest scan recorded no waits. Call waits with now=true to sample them.".into()];
            if let Some(reason)=snapshot.source_problem(&[culprit_core::source_id::DOCDB_ACTIVITY_HISTORY]) {notes.push(reason);}
            notes
        }),
        "signals": snapshot.pressure.as_ref().map(|p| p.signals.iter().map(signal).collect::<Vec<_>>()).unwrap_or_default(),
        "storage": snapshot.pressure.as_ref().map(|p| p.files.iter().take(12).map(file).collect::<Vec<_>>()).unwrap_or_default(),
        "replication": snapshot.pressure.as_ref().map(|p| p.replicas.clone()).unwrap_or_default(),
        "operations": snapshot.operations.counters_only(),
        "oldest_open_transaction": snapshot.pressure.as_ref().and_then(|p| p.oldest_transaction.as_ref()).map(|t| json!({
            "session": t.session, "open_for": format::duration(t.seconds * 1000.0), "state": t.state_words(), "program": t.program,
        })),
    });
    statement_work::attach(&mut response, report);
    jobs::attach(&mut response, report);
    completed_spills::attach(&mut response, report);
    page_server_reads::attach(&mut response, report);
    connection_outcomes::attach(&mut response, report);
    counter_ratios::attach(&mut response, report);
    performance_counters::attach(&mut response, report);
    configured_log_rates::attach(&mut response, report);
    if let Some(native) = snapshot.pressure.as_ref().and_then(|pressure| pressure.replication.as_deref()) {
        match culprit_engine::explain_replication(native, true, REPLICATION_ENTRIES) {
            Ok(evidence) => {
                let (from, until) = evidence.context.bounds();
                response["replication_evidence"] = json!({
                    "title": evidence.title, "summary": evidence.summary,
                    "sources": evidence.sources, "read_complete": evidence.read_complete,
                    "read_context": {
                        "native": evidence.context.native().is_some(),
                        "observed_from": from, "observed_until": until,
                    },
                    "native_rows": evidence.native_rows, "observed_rows": evidence.observed_rows,
                    "state_rows": evidence.state_rows, "presented_rows": evidence.presented_rows,
                    "entries_limited": evidence.entries_limited,
                    "entries": evidence.entries, "limitations": evidence.limitations,
                });
            }
            Err(_) => {
                response["replication_evidence_unavailable"] =
                    json!(culprit_core::t!("mcp.waits.replication_unavailable"));
            }
        }
    }
    if let Some(compilation) = &report.query_compilation {
        response["query_compilation"] = json!({
            "title": compilation.title,
            "summary": compilation.summary,
            "source_state": compilation.source_state,
            "source_reason": compilation.source_reason,
            "entries_retained": compilation.entries.len(),
            "entries": compilation.entries.iter().take(COMPILATION_ENTRIES).collect::<Vec<_>>(),
            "limitations": compilation.limitations,
            "comparison": compilation.comparison.as_ref().map(|comparison| json!({
                "title":comparison.title,"summary":comparison.summary,
                "observed_from":comparison.observed_from,"observed_until":comparison.observed_until,
                "baseline":comparison.baseline.as_ref().map(|reference|json!({"id":reference.id,"taken_at":reference.taken_at})),
                "missing_before":comparison.missing_before,"missing_after":comparison.missing_after,
                "issues":comparison.issues,"entries_retained":comparison.entries.len(),
                "entries":comparison.entries.iter().take(COMPILATION_ENTRIES).collect::<Vec<_>>(),
                "limitations":comparison.limitations,
            })),
        });
    }
    if let Some(activity) = activity {
        response["activity"] = activity;
    }
    if let Some(admission) = &report.compiler_admission {
        response["compiler_admission"] = json!({
            "title": admission.title, "summary": admission.summary,
            "source_state": admission.source_state, "source_reason": admission.source_reason,
            "waiters_retained": admission.waiters.len(), "gateways_retained": admission.gateways.len(),
            "waiters": admission.waiters.iter().take(COMPILER_ADMISSION_ENTRIES).map(|entry| json!({
                "observation_key": entry.observation_key, "title": entry.title, "explanation": entry.explanation,
                "investigation": entry.investigation, "verify": entry.verify,
            })).collect::<Vec<_>>(),
            "gateways": admission.gateways.iter().take(COMPILER_ADMISSION_ENTRIES).map(|entry| json!({
                "observation_key": entry.observation_key, "title": entry.title, "explanation": entry.explanation,
            })).collect::<Vec<_>>(),
            "limitations": admission.limitations,
        });
    }
    if let Some(pages) = &report.page_waits {
        response["page_waits"] = json!({
            "title": pages.title,
            "summary": pages.summary,
            "source_state": pages.source_state,
            "source_reason": pages.source_reason,
            "entries_retained": pages.entries.len(),
            "entries": pages.entries.iter().take(PARTICIPANT_ENTRIES).collect::<Vec<_>>(),
            "limitations": pages.limitations,
        });
    }
    if let Some(tasks) = &report.task_space {
        response["task_space"] = json!({
            "title": tasks.title,
            "summary": tasks.summary,
            "source_state": tasks.source_state,
            "source_reason": tasks.source_reason,
            "entries_retained": tasks.entries.len(),
            "entries": tasks.entries.iter().take(PARTICIPANT_ENTRIES).collect::<Vec<_>>(),
            "limitations": tasks.limitations,
        });
    }
    if let Some(cleanup) = &report.cleanup {
        response["cleanup"] = json!({
            "title": cleanup.title,
            "summary": cleanup.summary,
            "sources": cleanup.sources,
            "entries_retained": cleanup.entries.len(),
            "entries": cleanup.entries.iter().take(CLEANUP_ENTRIES).collect::<Vec<_>>(),
            "boundaries": cleanup.boundaries.iter().map(|boundary| json!({
                "horizon": boundary.horizon,
                "complete": boundary.complete,
                "oldest_observed_retained": boundary.oldest_observed.len(),
                "oldest_observed": boundary.oldest_observed.iter().take(CLEANUP_BOUNDARY_REFERENCES).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "limitations": cleanup.limitations,
        });
    }
    if let Some(resources) = &snapshot.operations.node_resources {
        response["node_resources"] = json!({
            "source":resources.source,"observed_from":resources.observed_from,"observed_until":resources.observed_until,
            "nodes_expected":resources.nodes_expected,"nodes_retained":resources.nodes.len(),"nodes_available":resources.nodes.iter().filter(|n|n.available).count(),
            "partial":resources.partial,"notes":resources.notes,"nodes":resources.nodes.iter().filter(|n|!n.available).chain(resources.nodes.iter().filter(|n|n.available)).take(12).collect::<Vec<_>>()
        });
    } else if let Some(reason) = snapshot.source_problem(&[culprit_core::source_id::DOCDB_NODE_RESOURCES]) {
        response["node_resources_unavailable"] = json!(reason);
    }
    if let Some(history) = &snapshot.operations.termination_history {
        let mut by_reason = std::collections::BTreeMap::<String, usize>::new();
        for event in &history.events {
            *by_reason.entry(culprit_core::wire_name(&event.reason)).or_default() += 1;
        }
        response["terminations"] = json!({
            "source":history.source,"requested_start":history.requested_start,"requested_end":history.requested_end,
            "scope":history.scope,"nodes":history.nodes,"visibility":history.visibility,"partial":history.partial,"notes":history.notes,
            "records_retained":history.events.len(),"by_reason":by_reason,"events":history.events.iter().take(12).collect::<Vec<_>>()
        });
    } else if let Some(reason) = snapshot.source_problem(&[culprit_core::source_id::DOCDB_TERMINATIONS]) {
        response["terminations_unavailable"] = json!(reason);
    }
    response
}

pub fn sample(s: &WaitSample) -> Value {
    json!({
        "seconds": s.seconds.round(),
        "method": s.method,
        "categories": s.categories.iter().map(|c| {
            let key = culprit_core::waits::category_key(&c.category);
            json!({
                "category": culprit_core::waits::category_info(key, s.engine).0,
                "share": format::percent(if s.total_ms > 0.0 { c.wait_ms / s.total_ms } else { 0.0 }),
                "time": format::duration(c.wait_ms),
                "top_wait": c.wait_type,
            })
        }).collect::<Vec<_>>(),
        // An empty sum is -0.0; a count of sessions is never negative.
        "busy_sessions_on_average": s.active_sessions.map(|v| v.max(0.0)),
        "signals": s.signals.iter().map(signal).collect::<Vec<_>>(),
        "notes": s.notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use culprit_core::{Engine, SignalScope, SignalUnit};

    #[test]
    fn participant_views_bound_entries_and_retain_task_identity_without_raw_payloads() {
        use culprit_core::{
            ObservedWaitResource, PageWaitExplanation, PageWaitReport, PageWaitTaskReference, SourceState,
            TaskAllocationActivity, TaskSpaceExplanation, TaskSpaceReport, WaitResourceKind, telemetry::*,
        };
        let mut data: Report = serde_json::from_str(include_str!("../../../fixtures/mssql-demo.report.json")).unwrap();
        let key = ActivityKey {
            session: NativeActivityIdentity { id: "42".into(), generation: "login-generation".into() },
            request: Some(NativeActivityIdentity { id: "0".into(), generation: "request-generation".into() }),
            transaction: None,
        };
        data.page_waits = Some(PageWaitReport {
            source_state: Some(SourceState::Partial),
            source_reason: Some("Native task visibility is partial.".into()),
            title: "Page waits".into(),
            summary: "Distinct task waits on one request.".into(),
            limitations: vec!["Task addresses do not establish an owner.".into()],
            entries: (0..PARTICIPANT_ENTRIES + 2)
                .map(|index| PageWaitExplanation {
                    observation_key: format!("exact-page-proof-{index}"),
                    request_key: key.clone(),
                    task: Some(PageWaitTaskReference {
                        task_address: format!("{index:016x}"),
                        execution_context_id: index as u32,
                    }),
                    resource: ObservedWaitResource {
                        kind: WaitResourceKind::MemoryPage,
                        native: NativeIdentity::new("5:1:42"),
                    },
                    title: "Observed task wait".into(),
                    blocking: culprit_core::PageWaitBlockingParticipation::unavailable(),
                    explanation: "Coordinator context is separate.".into(),
                    investigation: "Inspect qualified participants.".into(),
                    verify: "Compare representative load.".into(),
                })
                .collect(),
        });
        data.task_space = Some(TaskSpaceReport {
            source_state: None,
            source_reason: Some("Source completeness is unknown.".into()),
            title: "Temporary activity".into(),
            summary: "Native allocation classes remain separate.".into(),
            limitations: vec!["No allocator or footprint established.".into()],
            entries: (0..PARTICIPANT_ENTRIES + 3)
                .map(|index| TaskSpaceExplanation {
                    observation_key: format!("exact-task-proof-{index}"),
                    task: TaskSpaceIdentity {
                        session_id: "42".into(),
                        request_id: "0".into(),
                        execution_context_id: index.to_string(),
                    },
                    activity: TaskAllocationActivity::InternalObjects,
                    title: "Internal-object activity".into(),
                    explanation: "No spilled operator established.".into(),
                    investigation: "Inspect retained actual operators.".into(),
                    verify: "Preserve native counter scope.".into(),
                })
                .collect(),
        });
        data.snapshot.operations.page_waits = Some(PageWaitSnapshot {
            source: "private-raw-page-source-canary".into(),
            observed_from: "2026-11-13T10:00:00Z".into(),
            observed_until: "2026-11-13T10:00:01Z".into(),
            observations: vec![],
        });
        data.snapshot.operations.task_space = Some(TaskSpaceSnapshot {
            source: "private-raw-task-source-canary".into(),
            observed_from: "2026-11-13T10:00:00Z".into(),
            observed_until: "2026-11-13T10:00:01Z".into(),
            observations: vec![],
        });
        let response = report(&data);
        for (name, count) in [("page_waits", PARTICIPANT_ENTRIES + 2), ("task_space", PARTICIPANT_ENTRIES + 3)] {
            assert_eq!(response[name]["entries_retained"], count);
            assert_eq!(response[name]["entries"].as_array().unwrap().len(), PARTICIPANT_ENTRIES);
            assert!(response["operations"].get(name).is_none());
        }
        let pages = &response["page_waits"];
        assert_eq!(pages["source_state"], "partial");
        assert_eq!(pages["source_reason"], "Native task visibility is partial.");
        for (index, entry) in pages["entries"].as_array().unwrap().iter().enumerate() {
            assert_eq!(entry["task"]["execution_context_id"], index);
            assert_eq!(entry["task"]["task_address"], format!("{index:016x}"));
            assert_eq!(entry["request_key"]["request"]["generation"], "request-generation");
            assert_eq!(entry["resource"]["native"]["id"], "5:1:42");
        }
        assert!(response["task_space"]["source_state"].is_null());
        assert_eq!(response["task_space"]["entries"][0]["activity"], "internal-objects");
        let text = serde_json::to_string(&response).unwrap();
        assert!(!text.contains("private-raw-page-source-canary"));
        assert!(!text.contains("private-raw-task-source-canary"));
    }

    #[test]
    fn empty_participant_views_preserve_unknown_partial_and_unavailable_coverage() {
        use culprit_core::{PageWaitReport, SourceState, TaskSpaceReport};
        let mut data: Report = serde_json::from_str(include_str!("../../../fixtures/mssql-demo.report.json")).unwrap();
        for state in [None, Some(SourceState::Partial), Some(SourceState::Unavailable), Some(SourceState::Empty)] {
            data.page_waits = Some(PageWaitReport {
                source_state: state,
                source_reason: Some("Native scope qualification.".into()),
                title: "Page waits".into(),
                summary: "No retained page rows.".into(),
                limitations: vec!["Missing task visibility is not zero.".into()],
                entries: vec![],
            });
            data.task_space = Some(TaskSpaceReport {
                source_state: state,
                source_reason: Some("Native counter qualification.".into()),
                title: "Temporary activity".into(),
                summary: "No retained task rows.".into(),
                limitations: vec!["Unknown counters are not zero.".into()],
                entries: vec![],
            });
            let response = report(&data);
            for name in ["page_waits", "task_space"] {
                assert_eq!(response[name]["source_state"], serde_json::to_value(state).unwrap());
                assert_eq!(response[name]["entries_retained"], 0);
                assert_eq!(response[name]["entries"], json!([]));
                assert_eq!(response[name]["limitations"].as_array().unwrap().len(), 1);
            }
            assert_eq!(response["page_waits"]["source_reason"], "Native scope qualification.");
            assert_eq!(response["task_space"]["source_reason"], "Native counter qualification.");
        }
    }

    fn compilation_report_data(entries: usize) -> Report {
        use culprit_core::{
            CompilationExplanation, CompilationMemory, CompilationReport, Metric, ResetMarker, key, t, telemetry::*,
        };
        let mut data: Report = serde_json::from_str(include_str!("../../../fixtures/mssql-demo.report.json")).unwrap();
        let source = "private-raw-compilation-source-canary";
        let observations = (0..entries)
            .map(|index| {
                let native = MssqlQueryCompilation {
                    statement: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
                    origin: StatementOrigin::Unknown,
                    identity: NativeQueryIdentity {
                        query_id: (index + 100).to_string(),
                        qualifiers: [
                            ("context_settings_id".into(), "5".into()),
                            ("object_id".into(), "1999".into()),
                            ("query_hash".into(), "0123456789ABCDEF".into()),
                            ("initial_compile_at".into(), "2020-01-01T06:00:00.1533333Z".into()),
                        ]
                        .into(),
                    },
                    compilations: "9007199254740993".into(),
                    last_compile_at: "2020-01-01T11:59:00.1533333Z".into(),
                    average_duration_us: 1500.5,
                    timings: Default::default(),
                    memory: Some(CompilationMemory {
                        average_kb: Some(2048.5),
                        last_kb: Some(0.0),
                        maximum_kb: Some(16384.0),
                    }),
                    seconds_since_first_compile: 21600,
                    reset: ResetMarker::Unknown,
                };
                OperationalObservation {
                    source: source.into(),
                    key: native.key().unwrap(),
                    fact: OperationalFact::MssqlQueryCompilation(Box::new(native)),
                }
            })
            .collect::<Vec<_>>();
        let explained = observations
            .iter()
            .map(|row| {
                let OperationalFact::MssqlQueryCompilation(native) = &row.fact else { panic!() };
                CompilationExplanation {
                    statement: None,
                    observation_key: row.key.clone(),
                    identity: native.identity.clone(),
                    title: t!("view.compilation.entry.title", query = &native.identity.query_id),
                    explanation: t!("view.compilation.entry.multiple", count = &native.compilations),
                    metrics: vec![
                        Metric::said_text(key!("view.compilation.metric.count"), native.compilations.clone()),
                        Metric::said_text(key!("common.metric.compile_memory_last"), "0 KiB"),
                        Metric::said_text(key!("common.metric.compile_memory_maximum"), "16384 KiB"),
                    ],
                    investigation: t!("view.compilation.investigate.multiple"),
                    verify: t!("view.compilation.verify"),
                }
            })
            .collect();
        let raw = CompilationSnapshot {
            context: None,
            source: source.into(),
            observed_from: "2026-11-13T10:00:00Z".into(),
            observed_until: "2026-11-13T10:00:01Z".into(),
            observations,
        };
        assert!(raw.is_valid());
        data.snapshot.operations.query_compilation = Some(raw);
        data.snapshot.workload.as_mut().unwrap().queries[0].text =
            "SELECT private_value FROM compilation_sql_body_canary".into();
        data.query_compilation = Some(CompilationReport {
            definition_read: None,
            comparison: None,
            source_state: None,
            source_reason: Some("native visibility was not qualified".into()),
            title: t!("view.compilation.title"),
            summary: t!("view.compilation.summary.entries", n = entries, count = entries.to_string()),
            limitations: vec![
                t!("view.compilation.limit.source_unknown"),
                t!("check.planning_time.caveat.compile_memory"),
            ],
            entries: explained,
        });
        data
    }

    #[test]
    fn compilation_output_bounds_entries_preserves_exact_metrics_and_omits_raw_native_and_sql_bodies() {
        let data = compilation_report_data(COMPILATION_ENTRIES + 3);
        let original = data.query_compilation.as_ref().unwrap();
        let response = report(&data);
        let compilation = &response["query_compilation"];
        assert_eq!(compilation["entries_retained"], original.entries.len());
        assert_eq!(compilation["entries"].as_array().unwrap().len(), COMPILATION_ENTRIES);
        assert_eq!(compilation["source_state"], Value::Null);
        assert_eq!(compilation["source_reason"], "native visibility was not qualified");
        assert_eq!(compilation["limitations"], serde_json::to_value(&original.limitations).unwrap());
        for (index, entry) in compilation["entries"].as_array().unwrap().iter().enumerate() {
            assert_eq!(entry, &serde_json::to_value(&original.entries[index]).unwrap());
            let metrics = entry["metrics"].as_array().unwrap();
            assert_eq!(
                metrics.iter().find(|metric| metric["key"] == "view.compilation.metric.count").unwrap()["text"],
                "9007199254740993"
            );
            assert_eq!(
                metrics.iter().find(|metric| metric["key"] == "common.metric.compile_memory_last").unwrap()["text"],
                "0 KiB"
            );
            assert!(entry.get("fact").is_none() && entry.get("observations").is_none());
            assert!(entry.get("memory_owner").is_none() && entry.get("current_memory").is_none());
        }
        assert!(response["operations"].get("query_compilation").is_none());
        let body = serde_json::to_string(compilation).unwrap();
        assert!(!body.contains("private-raw-compilation-source-canary"));
        assert!(!body.contains("compilation_sql_body_canary"));
        assert!(!body.contains("average_duration_us") && !body.contains("seconds_since_first_compile"));
        assert!(body.len() < 20_000);
    }

    #[test]
    fn compact_compilation_output_preserves_empty_partial_and_unavailable_source_qualifications() {
        for state in [
            culprit_core::SourceState::Empty,
            culprit_core::SourceState::Partial,
            culprit_core::SourceState::Unavailable,
        ] {
            let mut data = compilation_report_data(0);
            let original = data.query_compilation.as_mut().unwrap();
            original.source_state = Some(state);
            let response = report(&data);
            let compilation = &response["query_compilation"];
            assert_eq!(compilation["source_state"], serde_json::to_value(state).unwrap());
            assert_eq!(compilation["source_reason"], "native visibility was not qualified");
            assert_eq!(compilation["entries_retained"], 0);
            assert_eq!(compilation["entries"], json!([]));
            assert!(response["operations"].get("query_compilation").is_none());
        }
    }

    #[test]
    fn compilation_comparisons_are_bounded_exact_and_do_not_repeat_the_original_native_namespace() {
        use culprit_core::{
            CompilationChange, CompilationComparisonIssue, CompilationComparisonReport, DatabaseIdentity, Engine,
            ScanReference,
        };
        let mut data = compilation_report_data(COMPILATION_ENTRIES + 3);
        let mut database = DatabaseIdentity::server(Engine::Mssql, "db", "private-prior-native-host", 1433, None);
        database.incarnation = Some("private-prior-native-incarnation".into());
        let compilation = data.query_compilation.as_mut().unwrap();
        compilation.comparison = Some(CompilationComparisonReport {
            baseline: Some(ScanReference {
                id: Some("original-scan-id".into()),
                taken_at: "2026-11-13T09:59:00Z".into(),
                database: Some(database),
            }),
            observed_from: Some("2026-11-13T09:59:00.000000001Z".into()),
            observed_until: Some("2026-11-13T10:00:01.000000002Z".into()),
            missing_before: 2,
            missing_after: 3,
            issues: vec![CompilationComparisonIssue::PartialRead, CompilationComparisonIssue::UnmatchedEntries],
            title: "Recorded compilation changes".into(),
            summary: "Present native entries compared; missing entries remain unknown.".into(),
            limitations: vec!["Counter changes do not measure interval duration or memory ownership.".into()],
            entries: compilation
                .entries
                .iter()
                .map(|entry| CompilationChange {
                    observation_key: entry.observation_key.clone(),
                    baseline_observation_key: entry.observation_key.clone(),
                    identity: entry.identity.clone(),
                    compilations: "18446744073709551615".into(),
                    last_compile_at: "2026-11-13T10:00:00.000000001Z".into(),
                    explanation: "A native count change was recorded.".into(),
                    investigation: "Inspect the original native entries.".into(),
                    verify: "Compare naturally collected observations.".into(),
                })
                .collect(),
        });
        let response = report(&data);
        let output = &response["query_compilation"]["comparison"];
        assert_eq!(output["entries_retained"], COMPILATION_ENTRIES + 3);
        assert_eq!(output["entries"].as_array().unwrap().len(), COMPILATION_ENTRIES);
        assert_eq!(output["entries"][0]["compilations"], "18446744073709551615");
        assert_eq!(output["observed_until"], "2026-11-13T10:00:01.000000002Z");
        assert_eq!(output["missing_before"], 2);
        assert_eq!(output["missing_after"], 3);
        assert_eq!(output["issues"], json!(["partial-read", "unmatched-entries"]));
        assert_eq!(output["baseline"], json!({"id":"original-scan-id","taken_at":"2026-11-13T09:59:00Z"}));
        let body = serde_json::to_string(output).unwrap();
        for private in [
            "private-prior-native-host",
            "private-prior-native-incarnation",
            "private-raw-compilation-source-canary",
            "compilation_sql_body_canary",
        ] {
            assert!(!body.contains(private), "{private}");
        }
        assert!(body.len() < 20_000);
    }

    #[test]
    fn cleanup_output_bounds_entries_and_tied_boundary_references_without_repeating_raw_context() {
        use culprit_core::{
            CleanupBoundary, CleanupExplanation, CleanupMechanism, CleanupReference, CleanupReport, Metric,
            SourceOutcome, SourceState, t, telemetry::*,
        };
        let mut data: Report = serde_json::from_str(include_str!("../../../fixtures/mssql-demo.report.json")).unwrap();
        let references = (0..1000)
            .map(|index| CleanupReference {
                source: "native-retainer-source".into(),
                observation_key: digest(format!("native-holder-{index}").as_bytes()),
                identity: Some(
                    NativeIdentity::new(format!("holder-{index}")).qualified("generation", "native-generation"),
                ),
            })
            .collect::<Vec<_>>();
        let entries = references
            .iter()
            .take(CLEANUP_ENTRIES + 3)
            .map(|reference| CleanupExplanation {
                reference: reference.clone(),
                mechanisms: vec![CleanupMechanism::ReplicationSlotHorizon],
                title: t!("view.cleanup.retainer.title", name = reference.identity.as_ref().unwrap().id.as_str()),
                explanation: t!("view.cleanup.retainer.slot"),
                metrics: vec![
                    Metric::text("Native transaction distance", "9007199254740993").keyed("native-distance:xmin"),
                ],
                investigation: t!("view.cleanup.retainer.investigate"),
                verify: t!("view.cleanup.retainer.verify"),
            })
            .collect::<Vec<_>>();
        data.cleanup = Some(CleanupReport {
            sources: vec![SourceOutcome {
                source: "native-retainer-source".into(),
                state: SourceState::Partial,
                reason: Some("Native visibility remains partial.".into()),
                reason_message: None,
            }],
            title: t!("view.cleanup.title"),
            summary: t!("view.cleanup.summary.entries", n = entries.len(), count = entries.len().to_string()),
            limitations: vec![t!("view.cleanup.limit.ownership"), t!("view.cleanup.limit.horizon_units")],
            boundaries: vec![CleanupBoundary {
                horizon: CleanupHorizon::Data,
                complete: false,
                oldest_observed: references,
            }],
            entries,
        });
        data.snapshot.operations.cleanup = vec![CleanupSnapshot {
            source: "private-cleanup-body-source-canary".into(),
            observed_from: "2026-11-13T10:00:00Z".into(),
            observed_until: "2026-11-13T10:00:01Z".into(),
            observations: vec![OperationalObservation {
                source: "private-cleanup-body-source-canary".into(),
                key: "oldest-cleanup-transaction".into(),
                fact: OperationalFact::MssqlCleanupHorizon(MssqlCleanupHorizon {
                    oldest_confirmed: Some(Box::new(MssqlCleanupTransaction {
                        session: NativeActivityIdentity {
                            id: "62".into(),
                            generation: "private-session-generation-canary".into(),
                        },
                        transaction: NativeActivityIdentity {
                            id: "42".into(),
                            generation: "2026-11-13T09:00:00.003".into(),
                        },
                        observed_time_without_offset: "2026-11-13T10:00:00.003".into(),
                        age_seconds: Some("3600".into()),
                        program: Some("private-cleanup-program-canary".into()),
                        native_status: None,
                        log_record_count: Some("1".into()),
                        log_bytes_used: None,
                        begin_lsn: None,
                        row_version_participant: false,
                        snapshot_isolation: None,
                        transaction_sequence_number: None,
                        versioning_age_seconds: None,
                    })),
                    oldest_unattributed_versioning: None,
                }),
            }],
        }];
        assert!(data.snapshot.operations.cleanup[0].is_valid());
        data.snapshot.workload.as_mut().unwrap().queries[0].text =
            "SELECT private_value FROM cleanup_sql_body_canary".into();
        let original = data.cleanup.as_ref().unwrap();
        let response = report(&data);
        let cleanup = &response["cleanup"];
        assert_eq!(cleanup["entries_retained"], CLEANUP_ENTRIES + 3);
        assert_eq!(cleanup["entries"].as_array().unwrap().len(), CLEANUP_ENTRIES);
        assert_eq!(cleanup["sources"], serde_json::to_value(&original.sources).unwrap());
        assert_eq!(cleanup["limitations"], serde_json::to_value(&original.limitations).unwrap());
        let boundary = &cleanup["boundaries"][0];
        assert_eq!(boundary["complete"], false);
        assert_eq!(boundary["oldest_observed_retained"], 1000);
        assert_eq!(boundary["oldest_observed"].as_array().unwrap().len(), CLEANUP_BOUNDARY_REFERENCES);
        assert_eq!(
            boundary["oldest_observed"][0],
            serde_json::to_value(&original.boundaries[0].oldest_observed[0]).unwrap()
        );
        for (index, entry) in cleanup["entries"].as_array().unwrap().iter().enumerate() {
            assert_eq!(entry, &serde_json::to_value(&original.entries[index]).unwrap());
            assert_eq!(entry["metrics"][0]["text"], "9007199254740993");
        }
        assert!(response["operations"].get("cleanup").is_none());
        for field in ["observations", "snapshot", "operations", "oldest_confirmed"] {
            assert!(cleanup.get(field).is_none(), "compact cleanup must omit the raw {field} container");
        }
        let compact = serde_json::to_string(cleanup).unwrap();
        for canary in [
            "private-cleanup-body-source-canary",
            "private-session-generation-canary",
            "private-cleanup-program-canary",
            "cleanup_sql_body_canary",
        ] {
            assert!(!compact.contains(canary), "compact cleanup must omit raw payload canary {canary}");
        }
        assert!(compact.len() < 20_000, "tied references must not expand compact output without a bound");
    }

    #[test]
    fn compact_cleanup_preserves_empty_partial_and_unavailable_source_qualifications() {
        use culprit_core::{CleanupReport, SourceOutcome, SourceState, t};
        for state in [SourceState::Partial, SourceState::Unavailable, SourceState::Empty] {
            let mut data: Report =
                serde_json::from_str(include_str!("../../../fixtures/mssql-demo.report.json")).unwrap();
            data.cleanup = Some(CleanupReport {
                sources: vec![SourceOutcome {
                    source: "explicit-native-cleanup-source".into(),
                    state,
                    reason: Some("Native source qualification.".into()),
                    reason_message: None,
                }],
                title: t!("view.cleanup.title"),
                summary: t!("view.cleanup.summary.empty"),
                limitations: vec![t!("view.cleanup.limit.point")],
                boundaries: vec![],
                entries: vec![],
            });
            let response = report(&data);
            let cleanup = &response["cleanup"];
            assert_eq!(cleanup["sources"], serde_json::to_value(&data.cleanup.as_ref().unwrap().sources).unwrap());
            assert_eq!(cleanup["sources"][0]["state"], serde_json::to_value(state).unwrap());
            assert_eq!(cleanup["sources"][0]["reason"], "Native source qualification.");
            assert_eq!(cleanup["entries_retained"], 0);
            assert_eq!(cleanup["entries"], json!([]));
            assert_eq!(cleanup["boundaries"], json!([]));
            assert!(response["operations"].get("cleanup").is_none());
        }
    }

    #[test]
    fn accumulated_parallel_waits_keep_native_totals_without_an_elapsed_share() {
        let mut data: Report = serde_json::from_str(include_str!("../../../fixtures/mssql-demo.report.json")).unwrap();
        let workload = data.snapshot.workload.as_mut().unwrap();
        workload.queries.truncate(1);
        let query = &mut workload.queries[0];
        query.text = "SELECT id FROM shop.orders".into();
        query.total_time_ms = 1000.0;
        query.executions = 2.0;
        query.waits = vec![culprit_core::WaitStat {
            mechanism: None,
            category: "Lock".into(),
            wait_type: None,
            wait_ms: 4000.0,
            tasks: None,
        }];
        let rendered = report(&data);
        let category = rendered["categories"].as_array().unwrap().iter().find(|c| c["key"] == "lock").unwrap();
        let query = &category["queries"][0];
        assert_eq!(query["recorded_ms"], 4000.0);
        assert_eq!(query["mean_recorded_ms"], 2000.0);
        assert!(query.get("share_of_query_time").is_none());
        assert!(
            query.get("share_of_category").is_none(),
            "independently captured category totals are not a denominator"
        );
    }

    #[test]
    fn file_averages_share_storage_precision_and_leave_untimed_operations_unknown() {
        let report: Report = serde_json::from_str(include_str!("../../../fixtures/mssql-demo.report.json")).unwrap();
        let mut source = report.snapshot.pressure.unwrap().files[0].clone();
        source.reads = 100.0;
        source.read_ms = Some(50.0);
        source.writes = 100.0;
        source.write_ms = Some(0.0);
        let rendered = file(&source);
        assert_eq!(rendered["avg_read"], "500 µs");
        assert_eq!(rendered["avg_write"], "0 ms");
        source.read_ms = None;
        source.writes = 0.0;
        let rendered = file(&source);
        assert!(rendered["avg_read"].is_null());
        assert!(rendered["avg_write"].is_null());
    }

    #[test]
    fn shared_node_resources_remain_bounded_and_separate_from_database_signals() {
        let mut data: Report =
            serde_json::from_str(include_str!("../../../fixtures/yugabyte-demo.report.json")).unwrap();
        data.snapshot.operations.node_resources = Some(culprit_core::NodeResources {
            source_message: None,
            note_messages: None,

            source: "shared native nodes".into(),
            observed_from: "2026-10-01T00:00:00Z".into(),
            observed_until: "2026-10-01T00:00:01Z".into(),
            nodes_expected: Some(10_001),
            partial: true,
            notes: vec!["Not database pressure".into()],
            nodes: (0..10_000)
                .map(|i| culprit_core::NodeResource {
                    node: format!("{i:032x}"),
                    available: true,
                    cpu_user_fraction: Some(0.99),
                    cpu_system_fraction: Some(0.01),
                    host_memory_total_bytes: None,
                    host_memory_available_bytes: None,
                    host_memory_free_bytes: None,
                    process_memory: None,
                })
                .collect(),
        });
        let resources = data.snapshot.operations.node_resources.as_mut().unwrap();
        let mut failed = resources.nodes[0].clone();
        failed.node = "ffffffffffffffffffffffffffffffff".into();
        failed.available = false;
        failed.cpu_user_fraction = None;
        failed.cpu_system_fraction = None;
        resources.nodes.push(failed);
        let value = report(&data);
        let resources = &value["node_resources"];
        assert_eq!(resources["nodes"].as_array().unwrap().len(), 12);
        assert_eq!(resources["nodes_retained"], 10_001);
        assert_eq!(resources["nodes_available"], 10_000);
        assert_eq!(resources["nodes"][0]["available"], false, "failed nodes must survive the presentation limit");
        assert!(value["operations"].get("node_resources").is_none());
        assert_eq!(resources["nodes"][0]["host_memory_total_bytes"], Value::Null);
        assert!(serde_json::to_vec(resources).unwrap().len() < 6000);
    }

    #[test]
    fn termination_output_bounds_records_and_keeps_native_limits_without_repeating_history() {
        let mut data: Report =
            serde_json::from_str(include_str!("../../../fixtures/yugabyte-demo.report.json")).unwrap();
        data.snapshot.operations.termination_history = Some(culprit_core::TerminationHistory {
            source_message: None,
            visibility_message: None,
            note_messages: None,

            source: "native termination buffer".into(),
            requested_start: "2026-10-01T00:00:00Z".into(),
            requested_end: "2026-10-08T00:00:00Z".into(),
            scope: culprit_core::CounterScope::Node,
            nodes: vec!["sample-node".into()],
            visibility: "native login visibility".into(),
            partial: true,
            notes: vec!["No failure-rate denominator".into()],
            events: vec![
                culprit_core::QueryTermination {
                    query_id: None,
                    node: Some("sample-node".into()),
                    started_at: None,
                    ended_at: "2026-10-02T00:00:00Z".into(),
                    reason: culprit_core::TerminationReason::Sigkill
                };
                10_000
            ],
        });
        let history = data.snapshot.operations.termination_history.as_mut().unwrap();
        let mut older = history.events[0].clone();
        older.reason = culprit_core::TerminationReason::Sigsegv;
        history.events.push(older);
        let value = report(&data);
        let history = &value["terminations"];
        assert_eq!(history["events"].as_array().unwrap().len(), 12);
        assert_eq!(history["records_retained"], 10_001);
        assert_eq!(history["by_reason"]["sigkill"], 10_000);
        assert_eq!(
            history["by_reason"]["sigsegv"], 1,
            "older termination categories must survive the event-list limit"
        );
        assert_eq!(history["requested_start"], "2026-10-01T00:00:00Z");
        assert_eq!(history["partial"], true);
        assert!(value["operations"].get("termination_history").is_none());
        assert!(serde_json::to_vec(history).unwrap().len() < 5000);
    }

    #[test]
    fn activity_output_is_bounded_and_does_not_repeat_raw_history_or_invent_durations() {
        let mut data: Report =
            serde_json::from_str(include_str!("../../../fixtures/yugabyte-demo.report.json")).unwrap();
        data.snapshot.operations.activity_history = Some(culprit_core::ActivityHistory {
            period: culprit_core::ActivityPeriod {
                source_message: None,
                note_messages: None,

                source: "native test samples".into(),
                requested_start: "2026-10-01T00:00:00Z".into(),
                requested_end: "2026-10-08T00:00:00Z".into(),
                first_sample: None,
                last_sample: None,
                scope: culprit_core::CounterScope::Node,
                nodes: vec![],
                observations: 20_000.0,
                weighted_observations: 40_000.0,
                partial: false,
                notes: vec!["Task weights are not elapsed time".into()],
            },
            events: (0..20_000)
                .map(|i| culprit_core::ActivityEvent {
                    node: Some("recorded-node".into()),
                    category: "rpc".into(),
                    component: "YSQL".into(),
                    wait_event: "TableRead".into(),
                    query_id: Some((i + 100).to_string()),
                    observations: 1.0,
                    weighted_observations: 2.0,
                })
                .collect(),
        });
        let result = report(&data);
        assert!(result["activity"]["period"]["partial"].as_bool().unwrap());
        let category = &result["activity"]["categories"][0];
        assert_eq!(category["events"].as_array().unwrap().len(), 8);
        assert_eq!(category["queries"].as_array().unwrap().len(), 5);
        assert_eq!(category["weighted_observations"], 20_000.0);
        assert!(category.get("time").is_none());
        assert!(result["operations"].get("activity_history").is_none());
        assert!(serde_json::to_vec(&result).unwrap().len() < 20_000);
    }

    #[test]
    fn waits_consumers_receive_the_measured_value_without_display_rounding() {
        let mut report_data: Report =
            serde_json::from_str(include_str!("../../../fixtures/mssql-demo.report.json")).unwrap();
        for (value, unit) in [
            (0.004, SignalUnit::Percent),
            (0.000_001, SignalUnit::PerSecond),
            (0.004, SignalUnit::Milliseconds),
            (1e308, SignalUnit::Bytes),
        ] {
            let measured = Signal::new("measured", "Measured signal", value, unit, SignalScope::Sampled);
            report_data.snapshot.pressure.as_mut().unwrap().signals = vec![measured.clone()];
            let sampled = WaitSample {
                engine: Engine::Mssql,
                taken_at: report_data.snapshot.taken_at.clone(),
                seconds: 30.0,
                method: "Two counter readings".into(),
                total_ms: 0.0,
                categories: vec![],
                signals: vec![measured],
                active_sessions: None,
                notes: vec![],
            };
            for response in [report(&report_data), sample(&sampled)] {
                assert_eq!(response["signals"][0]["value"].as_f64(), Some(value));
                assert_eq!(response["signals"][0]["unit"], serde_json::to_value(unit).unwrap());
            }
        }
    }
}
