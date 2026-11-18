//! Backend-native counter vocabulary and selection live once; a name never substitutes for its native type.
use super::*;

pub const PERFORMANCE_COUNTER_PROFILE: &str = "sqlserver-performance-counters-v2";
pub const PERFORMANCE_COUNTER_PROFILE_PROPERTY: &str = "performance_profile";
pub const PERFORMANCE_COUNTER_CLOCK_PROPERTY: &str = "counter_observed_at";
pub const PERFORMANCE_COUNTER_ROWS_PROPERTY: &str = "native_counter_rows";
pub const PERFORMANCE_COUNTER_WITHHELD_PROPERTY: &str = "performance_fields_withheld";

#[derive(Clone, Copy, Debug)]
pub enum PerformanceCounterInstances {
    Uninstanced,
    Named(&'static [&'static str]),
    Any,
}
impl PerformanceCounterInstances {
    pub fn accepts(self, instance: Option<&str>) -> bool {
        match self {
            Self::Uninstanced => instance.is_none(),
            Self::Named(names) => instance.is_some_and(|instance| names.contains(&instance)),
            Self::Any => true,
        }
    }
}

/// Families describe native populations. They do not declare the selected database or a causal owner.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PerformanceCounterFamily {
    Compilation,
    Connections,
    BufferCache,
    WorkspaceMemory,
    PlanCache,
    TemporaryTables,
    TransactionLog,
    LocalSsdCache,
}

pub struct PerformanceCounterSpec {
    pub id: &'static str,
    pub object_family: Option<&'static str>,
    pub native_name: &'static str,
    pub instances: PerformanceCounterInstances,
    pub unit: CounterUnit,
    pub nature: CounterNature,
    pub family: PerformanceCounterFamily,
    pub label_key: &'static str,
    pub ratio: Option<CounterRatioKind>,
    /// The existing six application projections remain independent of newly retained informational counters.
    pub application: bool,
}
impl PerformanceCounterSpec {
    pub fn matches(&self, object: &str, name: &str, instance: Option<&str>) -> bool {
        self.native_name == name
            && self.object_family.is_none_or(|family| object.contains(family))
            && self.instances.accepts(instance)
    }
    pub fn meaning(&self, native_type: i32) -> (CounterNature, CounterUnit) {
        let nature = CounterNature::from_windows_type(native_type);
        if nature == self.nature { (nature, self.unit) } else { (CounterNature::Unknown, CounterUnit::Unknown) }
    }
}

pub struct PerformanceCounterProfile {
    pub native_object: &'static str,
    pub selection: &'static str,
    pub fields: &'static [PerformanceCounterSpec],
}
impl PerformanceCounterProfile {
    pub fn source_is_valid(&self, source: &StateSourceCoverage) -> bool {
        source.source == crate::source_id::PERFORMANCE_COUNTERS && source.is_valid()
    }
    pub fn candidate(&self, sample: &NativeCounterSet) -> bool {
        self.matches(sample)
            || sample.properties.contains_key(PERFORMANCE_COUNTER_WITHHELD_PROPERTY)
            || sample.counters.iter().any(|counter| {
                self.field(&counter.native_object, &counter.native_name, counter.native_instance.as_deref()).is_some()
            })
    }
    pub fn matches(&self, sample: &NativeCounterSet) -> bool {
        sample.identity.id == self.native_object
            && sample.identity.qualifiers.get("native_object").is_some_and(|value| value == self.native_object)
    }
    pub fn field(&self, object: &str, name: &str, instance: Option<&str>) -> Option<&PerformanceCounterSpec> {
        self.fields.iter().find(|field| field.matches(object, name, instance))
    }
    /// Known semantics require both this source's fixed descriptor and the recorded native counter type.
    /// Unknown native codes keep raw integers with Unknown nature/unit; contradictions never become repaired rows.
    pub fn validate(&self, sample: &NativeCounterSet) -> Result<()> {
        if !sample.is_valid() || !self.matches(sample) || sample.scope != crate::MeasurementScope::Server {
            return Err(CollectionStop::InvalidPage.error());
        }
        if sample.properties.get(PERFORMANCE_COUNTER_PROFILE_PROPERTY).is_some_and(|value| value != self.selection)
            || sample.counters.len() + sample.ratio_gaps.len() > MAX_NATIVE_COUNTERS
            || sample.counters.iter().any(|counter| counter.reset != crate::ResetMarker::Unknown)
        {
            return Err(CollectionStop::InvalidPage.error());
        }
        self.observed_at(sample)?;
        let mut native_rows = std::collections::BTreeSet::new();
        for counter in &sample.counters {
            let field = self
                .field(&counter.native_object, &counter.native_name, counter.native_instance.as_deref())
                .ok_or_else(|| CollectionStop::InvalidPage.error())?;
            let meaning = match counter.native_type.as_deref() {
                Some(raw) => {
                    let code = raw
                        .parse::<i32>()
                        .ok()
                        .filter(|code| code.to_string() == raw)
                        .ok_or_else(|| CollectionStop::InvalidPage.error())?;
                    field.meaning(code)
                }
                None => (CounterNature::Unknown, CounterUnit::Unknown),
            };
            if counter.value.integer().is_none_or(|value| i64::try_from(value).is_err())
                || !native_rows.insert((&counter.native_object, &counter.native_name, &counter.native_instance))
                || (counter.nature, counter.unit) != meaning
            {
                return Err(CollectionStop::InvalidPage.error());
            }
        }
        for gap in &sample.ratio_gaps {
            if !native_rows.insert((&gap.native_object, &gap.native_name, &gap.native_instance))
                || self
                    .field(&gap.native_object, &gap.native_name, gap.native_instance.as_deref())
                    .is_none_or(|field| field.ratio.is_none())
            {
                return Err(CollectionStop::InvalidPage.error());
            }
        }
        for ratio in &sample.ratios {
            if self.declares_selection(sample)
                && ratio.observed_at.as_deref().map(canonical_time).transpose()?.as_deref()
                    != self.observed_at(sample)?
            {
                return Err(CollectionStop::InvalidPage.error());
            }
            if ratio.kind == CounterRatioKind::PlanCacheHit && ratio.numerator.is_none() && ratio.base.is_none() {
                return Err(CollectionStop::InvalidPage.error());
            }
            for (key, role) in
                [(&ratio.numerator, CounterNature::FractionNumerator), (&ratio.base, CounterNature::FractionBase)]
            {
                let Some(key) = key else { continue };
                let descriptor = sample
                    .counters
                    .iter()
                    .find(|counter| &counter.key == key)
                    .map(|counter| (&counter.native_object, &counter.native_name, counter.native_instance.as_deref()))
                    .or_else(|| {
                        sample
                            .ratio_gaps
                            .iter()
                            .find(|gap| &gap.key == key)
                            .map(|gap| (&gap.native_object, &gap.native_name, gap.native_instance.as_deref()))
                    })
                    .ok_or_else(|| CollectionStop::InvalidPage.error())?;
                if self
                    .field(descriptor.0, descriptor.1, descriptor.2)
                    .is_none_or(|field| field.ratio != Some(ratio.kind) || field.nature != role)
                {
                    return Err(CollectionStop::InvalidPage.error());
                }
            }
        }
        self.native_rows(sample)?;
        Ok(())
    }
    pub fn native_rows(&self, sample: &NativeCounterSet) -> Result<Option<u32>> {
        let Some(raw) = sample.properties.get(PERFORMANCE_COUNTER_ROWS_PROPERTY) else { return Ok(None) };
        let rows = raw
            .parse::<u32>()
            .ok()
            .filter(|rows| rows.to_string() == *raw)
            .ok_or_else(|| CollectionStop::InvalidPage.error())?;
        if rows as usize != sample.counters.len() + sample.ratio_gaps.len() {
            return Err(CollectionStop::InvalidPage.error());
        }
        Ok(Some(rows))
    }
    pub fn observed_at<'a>(&self, sample: &'a NativeCounterSet) -> Result<Option<&'a str>> {
        let Some(at) = sample.properties.get(PERFORMANCE_COUNTER_CLOCK_PROPERTY) else { return Ok(None) };
        if at.len() > 64 || canonical_time(at).ok().as_deref() != Some(at.as_str()) {
            return Err(CollectionStop::InvalidPage.error());
        }
        Ok(Some(at))
    }
    pub fn declares_selection(&self, sample: &NativeCounterSet) -> bool {
        sample.properties.get(PERFORMANCE_COUNTER_PROFILE_PROPERTY).is_some_and(|value| value == self.selection)
    }
}

#[cfg(test)]
mod tests;
