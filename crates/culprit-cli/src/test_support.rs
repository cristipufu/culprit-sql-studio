//! Structural assertions shared by the CLI and its agent-facing output tests.
pub(crate) fn assert_no_json_keys(value: &serde_json::Value, forbidden: &[&str]) {
    match value {
        serde_json::Value::Object(fields) => {
            for (key, value) in fields {
                assert!(!forbidden.contains(&key.as_str()), "private receipt field {key}");
                assert_no_json_keys(value, forbidden);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                assert_no_json_keys(item, forbidden);
            }
        }
        _ => {}
    }
}
