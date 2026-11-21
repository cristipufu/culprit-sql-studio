use culprit_engine::LocalManagedInstanceResources;
use serde_json::{Value, json};
pub(super) fn captured(analysis: &LocalManagedInstanceResources) -> Value {
    json!({"analysis":"managed_instance_resources","historical_context":true,"collected_from":analysis.collected_from(),"collected_until":analysis.collected_until(),"local_verification_bytes":analysis.bytes_read(),"managed_instance_resources":crate::waits_view::managed_instance_resources::value(analysis.report())})
}
