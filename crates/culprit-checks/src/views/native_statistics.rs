//! Native statistic formatting preserves integer digits and the declared unit; it performs no size inference.
use culprit_core::{
    format, t,
    telemetry::{MeasurementValue, RuntimeMetricUnit},
};
pub(super) fn value(value: &MeasurementValue, unit: RuntimeMetricUnit, quantity_key: &str) -> String {
    match value {
        MeasurementValue::Integer(value) => quantity(format::digits(value.clone()), unit, quantity_key),
        MeasurementValue::Real(value) => number(*value, unit, quantity_key),
    }
}
pub(super) fn number(value: f64, unit: RuntimeMetricUnit, quantity_key: &str) -> String {
    quantity(format::digits(value.to_string()), unit, quantity_key)
}
fn quantity(value: String, unit: RuntimeMetricUnit, quantity_key: &str) -> String {
    let symbol = match unit {
        RuntimeMetricUnit::Picoseconds => "ps",
        RuntimeMetricUnit::Nanoseconds => "ns",
        RuntimeMetricUnit::Microseconds => "µs",
        RuntimeMetricUnit::Milliseconds => "ms",
        RuntimeMetricUnit::Seconds => "s",
        RuntimeMetricUnit::Bytes => "B",
        RuntimeMetricUnit::Pages { bytes_per_page } => {
            return t!(
                "view.runtime_fragments.pages",
                value = value,
                bytes = format::digits(bytes_per_page.to_string())
            );
        }
        RuntimeMetricUnit::Count | RuntimeMetricUnit::Ratio => return value,
    };
    culprit_core::i18n::t(quantity_key, &[("value", value.as_str().into()), ("unit", symbol.into())])
}
