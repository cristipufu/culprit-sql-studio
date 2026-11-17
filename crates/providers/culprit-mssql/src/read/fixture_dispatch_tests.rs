use super::*;
use culprit_core::telemetry::StatementOrigin;

#[test]
fn buffered_and_streaming_monitoring_descriptors_keep_real_tags_and_original_template_identity() {
    let original = "SELECT N'/* culprit */' AS literal_value; SELECT 2 AS second_value";
    let (streaming, _receiver) = Output::streaming(1, 1024);
    let native = Rec::of(&[("engine_internal", Val::Int(0)), ("cloud_internal", Val::Int(0))]);
    for output in [Output::buffered(), streaming] {
        let query = ReadQuery::new(original, vec![], 2, 1024, output, QueryDispatch::Monitoring);
        let provenance = crate::statement_origin::read(&native, Some(&query.sql));
        assert_eq!(provenance.origin, StatementOrigin::CulpritMonitoring);
        assert_eq!(provenance.template_id, Some(crate::statement_origin::template_id(original)));
        assert_eq!(query.sql.matches(crate::statement_origin::MONITORING_QUERY_MARKER).count(), 2);
        assert!(query.sql.contains("N'/* culprit */'"), "literal text remains literal, not an ownership marker");
    }
}
