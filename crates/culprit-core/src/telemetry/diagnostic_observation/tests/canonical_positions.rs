use super::*;

#[test]
fn a_real_activity_row_cannot_claim_a_position_outside_its_original_native_count() {
    for complete_scope in [true, false] {
        let original = frame(1, vec![actor()], complete_scope);
        let (reference, observation) = &original.2[0];
        let native = original.0.observation.activity().unwrap();
        assert_eq!(native.observations, 1);
        let record = TelemetryRecord::from(observation.clone());
        reference.validate_canonical_record(&original.0, &record).unwrap();
        DiagnosticActivityRow { reference, observation }.verify(&original.0, &original.1).unwrap();
        assert_eq!(observe(&original, None).revisions.len(), 1);

        for ordinal in [native.observations, u32::MAX] {
            let mut impossible = reference.clone();
            impossible.row_ordinal = ordinal;
            assert!(impossible.is_valid(), "only the source can bound a structurally valid ordinal");
            assert_eq!(impossible.capture_ref(), reference.capture_ref());
            assert!(impossible.validate_canonical_record(&original.0, &record).is_err(), "{ordinal}");
            let row = DiagnosticActivityRow { reference: &impossible, observation };
            assert!(row.verify(&original.0, &original.1).is_err(), "{ordinal}");
            assert!(
                observe_diagnostic_activity(
                    &original.0,
                    &original.1,
                    &[row],
                    None,
                    &ActivityPolicy::default(),
                    "2020-01-01T01:00:00Z",
                )
                .is_err(),
                "{ordinal}",
            );
        }
    }
}

#[test]
fn a_retained_later_position_uses_the_original_native_count_when_the_archive_body_is_incomplete() {
    let first = actor();
    let mut second = first.clone();
    second.key.session.id = "52".into();
    let original = frame(1, vec![first, second], true);
    let (reference, observation) = &original.2[1];
    assert_eq!(reference.row_ordinal, 1);
    reference.validate_canonical_record(&original.0, &TelemetryRecord::from(observation.clone())).unwrap();
    DiagnosticActivityRow { reference, observation }.verify(&original.0, &original.1).unwrap();
    let retained = [ActivityFrameRow { reference: Some(reference), observation }];
    let frame = ActivityFrameEvidence::new(&original.0, &retained, false).unwrap();
    assert!(!frame.read_complete());
    assert_eq!(frame.native().observations, 2);
    assert_eq!(frame.rows().count(), 1);
    assert_eq!(frame.row(&observation.key).unwrap().reference, Some(reference));
}
