use super::*;
use culprit_collector::analytical::test_support::{InstanceResourceCase, instance_resource_evidence};
use culprit_engine::LocalCaptureAnalysisKind;

#[tokio::test]
async fn processor_cpu_original_reaches_compact_mcp_without_private_epoch_or_another_source() {
    use culprit_collector::analytical::test_support::{ProcessCpuCase, instance_resource_cpu_evidence};
    for case in ProcessCpuCase::ALL {
        let (source, rows) = instance_resource_cpu_evidence(case);
        let epoch = match &source.partition {
            SourcePartition::InstanceLocal { epoch: Some(epoch), .. } => epoch.clone(),
            _ => panic!(),
        };
        let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::InstanceResources)
                .await;
        assert!(!error, "{case:?}:{value}");
        let cpu = &value["instance_resources"]["process_cpu"];
        assert!(!cpu.is_null());
        assert_eq!(cpu["native_source"]["source"], json!(culprit_core::source_id::PROCESSOR_TOPOLOGY));
        assert!(cpu["fields"].as_array().unwrap().len() <= 5);
        assert!(!cpu.to_string().contains(&epoch));
        assert!(!cpu.to_string().contains("manifest_sha256"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn exact_shared_instance_reports_reach_mcp_with_bounded_fields_original_clocks_and_no_private_scope() {
    for case in InstanceResourceCase::ALL {
        let (source, rows) = instance_resource_evidence(case);
        let native = source.observation.snapshot().unwrap().clone();
        let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::InstanceResources)
                .await;
        assert!(!error, "{case:?}:{value}");
        assert_eq!(value["independent_shared_scope"], true);
        assert_eq!(value["historical_context"], true);
        assert_eq!(value["read_complete"], true);
        assert_eq!(value["original_read_bounds"], json!({"from":native.observed_from,"until":native.observed_until}));
        let report = &value["instance_resources"];
        assert_eq!(report["source_state"], json!(native.sources[0].state));
        for group in report["groups"].as_array().unwrap() {
            assert!(group["fields"].as_array().unwrap().len() <= 5);
        }
        assert!(report["groups"].as_array().unwrap().len() <= 5);
        if case == InstanceResourceCase::Exact {
            assert!(value.to_string().contains("9007199254740993"));
        }
        for private in [
            "native-instance-namespace",
            "native-instance-a",
            "native-process-a",
            "manifest_sha256",
            "payload_sha256",
            "source_sha256",
            "qualifiers",
        ] {
            assert!(!value.to_string().contains(private), "{private}");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn individual_scheduler_originals_reach_compact_mcp_with_native_null_units_and_bounded_rows_without_provider_work()
 {
    use culprit_collector::analytical::test_support::{SchedulerCase, instance_resource_scheduler_evidence};
    for case in SchedulerCase::ALL {
        let (source, rows) = instance_resource_scheduler_evidence(case);
        let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::InstanceResources)
                .await;
        assert!(!error, "{case:?}:{value}");
        let schedulers = &value["instance_resources"]["schedulers"];
        assert_eq!(schedulers["native_source"]["source"], culprit_core::source_id::SCHEDULERS);
        assert_eq!(schedulers["read_complete"], true);
        assert!(schedulers["entries"].as_array().unwrap().len() <= 5);
        for entry in schedulers["entries"].as_array().unwrap() {
            assert_eq!(
                entry["fields"].as_array().unwrap().len(),
                culprit_core::telemetry::MSSQL_SCHEDULER_FIELDS.len() - 1
            );
            assert!(entry["fields"].as_array().unwrap().iter().any(|field| field["native_name"] == "work_queue_count"));
        }
        if matches!(case, SchedulerCase::Exact | SchedulerCase::SpinlockRawPositive) {
            let fields = schedulers["entries"][0]["fields"].as_array().unwrap();
            let spin = fields.iter().find(|field| field["native_name"] == "spinlock_wait_count").unwrap();
            assert_eq!(spin["availability"], "instrumentation-unqualified");
            assert!(spin["value"].is_null());
            assert_eq!(spin["unqualified"]["native_value"], if case == SchedulerCase::Exact { "0" } else { "7" });
            assert_eq!(spin["unqualified"]["required_global_trace_flag"], 8134);
        }
        if case == SchedulerCase::NativeNull {
            assert!(schedulers.to_string().contains("native-null"));
        }
        if case == SchedulerCase::ColumnUnavailable {
            assert!(schedulers.to_string().contains("column-unavailable"));
        }
        if case == SchedulerCase::Limited {
            assert_eq!(schedulers["entries_available"], 256);
            assert_eq!(schedulers["entries_omitted"], 251);
        }
        for private in ["native-process-a", "native-instance-a", "manifest_sha256", "source_sha256", "qualifiers"] {
            assert!(!schedulers.to_string().contains(private), "{private}");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn retained_scheduler_cpu_remainder_reaches_compact_mcp_from_original_inputs_without_provider_work() {
    use culprit_collector::analytical::test_support::{ProcessCpuIntervalCase, process_cpu_interval_evidence};
    for case in ProcessCpuIntervalCase::ALL {
        let [(source, rows), _] = process_cpu_interval_evidence(case);
        let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::InstanceResources)
                .await;
        assert!(!error, "{case:?}: {value}");
        let cpu = &value["instance_resources"]["process_cpu"];
        let history = &cpu["populations"][1];
        let entries = history["entries"].as_array().unwrap();
        if case == ProcessCpuIntervalCase::HistoryDenied {
            assert!(entries.is_empty());
        } else {
            let fields = entries[0]["fields"].as_array().unwrap();
            assert_eq!(fields.len(), 8);
            let other = fields.iter().find(|field| field["key"] == "other_process_cpu_percent").unwrap();
            assert_eq!(other["value"], json!(case.expected_history_other_percent().map(|value| value.to_string())));
            assert!(fields.iter().any(|field| field["key"] == "raw_system_idle"));
            assert!(fields.iter().any(|field| field["key"] == "raw_process_utilization"));
            if case == ProcessCpuIntervalCase::PartialHistory {
                assert_eq!(history["source_state"], "partial");
                assert_eq!(history["native_source"]["complete_scope"], false);
            }
        }
        assert!(!cpu.to_string().contains("manifest_sha256"));
        assert!(!cpu.to_string().contains("instance_epoch"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
