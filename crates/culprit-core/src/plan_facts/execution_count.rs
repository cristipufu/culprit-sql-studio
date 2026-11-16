//! Serial execution scope is source-issued; exact native counts never pass through binary64 on ingestion.
use serde::{Deserialize, Deserializer};

pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    Ok(Option::<String>::deserialize(deserializer)?.filter(|value| {
        value.len() <= 20 && value.parse::<u64>().is_ok_and(|count| count > 0 && count.to_string() == *value)
    }))
}

#[cfg(test)]
mod tests {
    use crate::PlanNodeFacts;

    #[test]
    fn serial_counts_preserve_exact_native_integers_and_reject_invalid_scope_counts() {
        for count in ["1", "10000", "9007199254740993", "18446744073709551615"] {
            let facts = PlanNodeFacts { serial_execution_count: Some(count.into()), ..Default::default() };
            let decoded: PlanNodeFacts = serde_json::from_value(serde_json::to_value(facts).unwrap()).unwrap();
            assert_eq!(decoded.serial_execution_count.as_deref(), Some(count));
        }
        for count in ["0", "01", "-1", "1.5", "1e3", "18446744073709551616"] {
            let mut facts = serde_json::to_value(PlanNodeFacts::default()).unwrap();
            facts["serial_execution_count"] = count.into();
            let decoded: PlanNodeFacts = serde_json::from_value(facts).unwrap();
            assert!(decoded.serial_execution_count.is_none(), "{count}");
        }
    }
}
