use super::*;
#[test]
fn empty_frozen_stream_finishes_and_rejects_a_second_read() {
    let mut stream = FrozenStream::new(TelemetryStream::ActivityObservations, vec![]);
    assert!(matches!(
        stream.page(None, &CollectionBudget::default()).unwrap(),
        CollectionRead::Page { next_cursor: None, .. }
    ));
    assert!(stream.page(None, &CollectionBudget::default()).is_err());
}
