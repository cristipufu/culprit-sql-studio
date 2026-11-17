//! The compiler gate retains its exact64 collector-owned population and original receipt name.
use super::*;
use crate::test_support::query_store_probes::{self, ProbePolicy};

pub(super) async fn seed(
    fixture: &OwnedSourceFixture,
    directory: &Path,
    cancellation: &CollectionCancellation,
) -> Result<BTreeMap<String, NativeQueryIdentity>> {
    query_store_probes::seed(fixture, directory, cancellation, 64, ProbePolicy::Collector, "compilation-native-setup")
        .await
}
