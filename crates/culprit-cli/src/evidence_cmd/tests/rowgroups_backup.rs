use super::*;
#[test]
fn selected_rowgroups_and_backup_archive_analyses_select_only_original_operational_rows() {
    for (name, kind) in [
        ("columnstore-rowgroups", Analysis::ColumnstoreRowgroups),
        ("backup-history", Analysis::BackupHistory),
        ("table-memory", Analysis::TableMemory),
    ] {
        for (options, format) in [(&["--json"][..], Format::Json), (&[][..], Format::Text)] {
            let args = analyze_args(name, options);
            assert!(args.analysis == kind);
            args.validate_selection().unwrap();
            let request = args.common.request(args.analysis.families(), format).unwrap();
            assert_eq!(request.families, [EvidenceFamily::OperationalSamples]);
            assert!(request.budget.recovery_payloads.is_none());
        }
        for option in [
            "--activity-range",
            "--definitions-range",
            "--compilation-baseline",
            "--cache-baseline",
            "--blocking-range",
        ] {
            assert!(analyze_args(name, &[option, &"a".repeat(64)]).validate_selection().is_err(), "{name}:{option}");
        }
    }
}
