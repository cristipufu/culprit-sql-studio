use super::{NativeLogLimits, NativeLogProblem, ParsedLogBatch, record};
use serde::Deserializer;
use serde::de::{DeserializeSeed, MapAccess, Visitor};
use serde_json::Value;
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    fmt,
};

const MAX_FIELDS: usize = 128;

pub(super) fn parse(input: &str, limits: NativeLogLimits) -> ParsedLogBatch {
    let mut batch = ParsedLogBatch::new();
    for line in input.split_inclusive('\n') {
        if line.len() > limits.record_bytes {
            batch.stopped = Some(NativeLogProblem::RecordBytes);
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if batch.records_examined as usize == limits.records {
            batch.stopped = Some(NativeLogProblem::RecordLimit);
            break;
        }
        batch.accept(fields(line, limits.field_bytes).and_then(record::decode));
    }
    batch
}

fn fields(input: &str, field_bytes: usize) -> std::result::Result<BTreeMap<String, Value>, NativeLogProblem> {
    let problem = Cell::new(None);
    let mut decoder = serde_json::Deserializer::from_str(input);
    let values = NativeFields { field_bytes, problem: &problem }
        .deserialize(&mut decoder)
        .map_err(|_| problem.take().unwrap_or(NativeLogProblem::MalformedRecord))?;
    decoder.end().map_err(|_| NativeLogProblem::MalformedRecord)?;
    Ok(values)
}

struct NativeFields<'a> {
    field_bytes: usize,
    problem: &'a Cell<Option<NativeLogProblem>>,
}
impl<'de> DeserializeSeed<'de> for NativeFields<'_> {
    type Value = BTreeMap<String, Value>;
    fn deserialize<D: Deserializer<'de>>(self, input: D) -> std::result::Result<Self::Value, D::Error> {
        input.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for NativeFields<'_> {
    type Value = BTreeMap<String, Value>;

    fn expecting(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("a native PostgreSQL JSON log object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut input: A) -> std::result::Result<Self::Value, A::Error> {
        let mut seen = BTreeSet::new();
        let mut values = BTreeMap::new();
        while let Some(key) = input.next_key::<String>()? {
            let problem = if seen.len() >= MAX_FIELDS || key.len() > self.field_bytes {
                Some(NativeLogProblem::FieldBytes)
            } else if !seen.insert(key.clone()) {
                Some(NativeLogProblem::DuplicateField)
            } else {
                None
            };
            if let Some(problem) = problem {
                self.problem.set(Some(problem));
                return Err(serde::de::Error::custom("bounded native field decoding stopped"));
            }
            if !record::known_field(&key) {
                input.next_value::<serde::de::IgnoredAny>()?;
                continue;
            }
            let value = input.next_value::<Value>()?;
            if value.as_str().is_some_and(|value| value.len() > self.field_bytes) {
                self.problem.set(Some(NativeLogProblem::FieldBytes));
                return Err(serde::de::Error::custom("bounded native field decoding stopped"));
            }
            values.insert(key, value);
        }
        Ok(values)
    }
}
