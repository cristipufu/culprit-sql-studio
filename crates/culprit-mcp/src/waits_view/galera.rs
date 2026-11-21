//! Bounded native values only; no original group/member/process identities or authenticated receipt internals.
use culprit_core::GaleraReport;
use serde_json::{Value, json};
pub(crate) fn value(report: &GaleraReport) -> Value {
    const ENTRY_LIMIT: usize = 5;
    let entries = report
        .entries
        .iter()
        .take(ENTRY_LIMIT)
        .map(|entry| {
            json!({
                "title":entry.title,"explanation":entry.explanation,
                "fields":GaleraReport::public_fields(entry).collect::<Vec<_>>(),
                "groups":entry.groups,"investigation":entry.investigation,"verify":entry.verify,
            })
        })
        .collect::<Vec<_>>();
    json!({"title":report.title,"summary":report.summary,"sources":report.sources,"limitations":report.limitations,
        "read_complete":report.read_complete,"native_rows":report.native_rows,"observed_rows":report.observed_rows,
        "entries_available":report.entries.len(),"entries_presented":entries.len(),"entries_omitted":report.entries.len()-entries.len(),
        "entries_limited":report.entries_limited || report.entries.len()>entries.len(),"entries":entries,
        "original_read_bounds":report.original.as_ref().map(|original|json!({"from":original.context.observed_from,"until":original.context.observed_until}))})
}
pub(super) fn attach(value: &mut Value, report: &culprit_core::Report) {
    if let Some(reports) = &report.galera {
        value["galera"] = json!(reports.iter().map(self::value).collect::<Vec<_>>());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use culprit_collector::analytical::test_support::{GaleraCase, galera_evidence};
    fn report(case: GaleraCase) -> GaleraReport {
        let (source, records) = galera_evidence(case);
        let rows = records
            .iter()
            .map(|row| {
                let culprit_core::telemetry::TelemetryRecord::Operational(row) = row else { panic!() };
                row
            })
            .collect::<Vec<_>>();
        culprit_checks::galera::explain(culprit_checks::galera::GaleraEvidence {
            source: &source,
            observations: &rows,
            read_complete: true,
        })
        .unwrap()
    }
    #[test]
    fn canonical_compact_original_keeps_availability_exact_values_and_clock_without_private_native_identity() {
        for case in GaleraCase::ALL {
            let report = report(case);
            let value = value(&report);
            assert_eq!(value["sources"], serde_json::to_value(&report.sources).unwrap());
            assert_eq!(value["native_rows"], report.native_rows.unwrap());
            assert_eq!(value["original_read_bounds"]["from"], "2020-01-01T00:00:00Z");
            assert!(value["entries"].as_array().unwrap().len() <= 5);
            if case == GaleraCase::Queued {
                assert_eq!(
                    value["entries"][0]["groups"][1]["fields"][0]["value"],
                    culprit_core::format::digits("9007199254740993".into())
                );
            }
            if case == GaleraCase::NativeNulls {
                assert!(value["entries"][0]["groups"][0]["fields"][0]["value"].is_null());
            }
            for private in [
                "aaaaaaaa-0000-4000-8000-000000000001",
                "bbbbbbbb-0000-4000-8000-000000000002",
                "private-galera-process-canary",
                "iFL88RG7XPipmQ5HOnA9iLqJw6w=",
                "manifest_sha256",
                "qualifiers",
            ] {
                assert!(!value.to_string().contains(private), "{private}");
            }
        }
    }
    #[test]
    fn unavailable_original_does_not_regain_clock_identity_or_zero_count_in_compact_output() {
        let report = culprit_checks::galera::unavailable(&culprit_core::message!("view.galera.sharing_withheld"));
        let value = value(&report);
        assert!(value["original_read_bounds"].is_null());
        assert!(value["native_rows"].is_null());
        assert!(value["entries"].as_array().unwrap().is_empty());
        assert_eq!(value["sources"][0]["state"], "unavailable");
    }
}
