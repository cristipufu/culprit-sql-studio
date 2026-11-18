use super::*;

#[test]
fn instance_resources_use_only_original_operational_bodies_and_the_existing_cumulative_read_budget() {
    for format in [Format::Json, Format::Text] {
        let args = analyze_args("instance-resources", &["--max-rows", "7", "--max-queries", "2"]);
        assert!(args.analysis == Analysis::InstanceResources);
        args.validate_selection().unwrap();
        let request = args.common.request(args.analysis.families(), format).unwrap();
        assert_eq!(request.families, [EvidenceFamily::OperationalSamples]);
        assert_eq!(request.budget.max_rows, 7);
        assert_eq!(request.budget.max_queries, 2);
    }
}
