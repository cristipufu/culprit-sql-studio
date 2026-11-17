//! Independent retained Query Store measurements. Native float averages are approximate; native bigint last
//! values remain exact strings. Stages need not exhaust compilation and are never summed into total CPU.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, Default, PartialEq)]
#[ts(export)]
pub struct MssqlCompilationTimings {
    pub last_duration_us: Option<String>,
    pub binding: MssqlCompilationStage,
    pub optimization: MssqlCompilationStage,
}
impl MssqlCompilationTimings {
    pub fn is_valid(&self) -> bool {
        exact(&self.last_duration_us) && self.binding.is_valid() && self.optimization.is_valid()
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, Default, PartialEq)]
#[ts(export)]
pub struct MssqlCompilationStage {
    pub average_duration_us: Option<f64>,
    pub last_duration_us: Option<String>,
    pub average_cpu_us: Option<f64>,
    pub last_cpu_us: Option<String>,
}
impl MssqlCompilationStage {
    fn is_valid(&self) -> bool {
        average(self.average_duration_us)
            && exact(&self.last_duration_us)
            && average(self.average_cpu_us)
            && exact(&self.last_cpu_us)
    }
}
fn average(value: Option<f64>) -> bool {
    value.is_none_or(|value| value.is_finite() && value >= 0.0)
}
fn exact(value: &Option<String>) -> bool {
    value.as_ref().is_none_or(|value| {
        value.len() <= 19 && value.parse::<i64>().is_ok_and(|number| number >= 0 && number.to_string() == *value)
    })
}

#[cfg(test)]
mod tests;
