//! Compiler admission is shared resource-pool context. A request can hold an earlier gate while waiting for another;
//! neither active counts across gates nor retained per-query memory peaks establish simultaneous allocations.
use crate::read::{
    NativeRead, Parameter, ReadError, ReadSession, boolean, integer, invalid_data, signed_integer, text,
};
use culprit_core::{Coverage, MeasurementScope, Result, Signal, SignalScope, SignalUnit, telemetry::*};

const NATIVE_OBJECT: &str = "sys.dm_exec_query_optimizer_memory_gateways";
const MAX_GATEWAYS: usize = 512;
const SQL: &str = r#"SELECT /* culprit */ TOP (@P1)
 pool_id,name,max_count,active_count,waiter_count,threshold_factor,threshold,is_active
 FROM sys.dm_exec_query_optimizer_memory_gateways ORDER BY pool_id,name OPTION(MAXDOP 1)"#;

pub(crate) struct Gateway {
    pool_id: u64,
    name: String,
    maximum: u64,
    active: u64,
    waiting: u64,
    threshold_factor: u64,
    threshold: i64,
    enabled: bool,
}

impl Gateway {
    fn decode(row: &crate::value::Rec) -> Result<Self> {
        let sample = Self {
            pool_id: integer(row, "pool_id")?.try_into().map_err(|_| invalid_data())?,
            name: text(row, "name")?,
            maximum: integer(row, "max_count")?.try_into().map_err(|_| invalid_data())?,
            active: integer(row, "active_count")?.try_into().map_err(|_| invalid_data())?,
            waiting: integer(row, "waiter_count")?.try_into().map_err(|_| invalid_data())?,
            threshold_factor: integer(row, "threshold_factor")?.try_into().map_err(|_| invalid_data())?,
            threshold: signed_integer(row, "threshold")?,
            enabled: boolean(row, "is_active")?,
        };
        if sample.threshold < -1 || !sample.counters().is_valid() {
            return Err(invalid_data());
        }
        Ok(sample)
    }

    fn counters(&self) -> NativeCounterSet {
        let mut counters = [
            ("maximum-compilers", "max_count", self.maximum),
            ("active-compilers", "active_count", self.active),
            ("waiting-compilers", "waiter_count", self.waiting),
        ]
        .into_iter()
        .map(|(key, name, value)| {
            NativeCounterSample::gauge(key, value, CounterUnit::Count, MeasurementScope::Server, NATIVE_OBJECT, name)
        })
        .collect::<Vec<_>>();
        if self.threshold >= 0 {
            counters.push(NativeCounterSample::gauge(
                "admission-threshold-bytes",
                self.threshold as u64,
                CounterUnit::Bytes,
                MeasurementScope::Server,
                NATIVE_OBJECT,
                "threshold",
            ));
        }
        NativeCounterSet {
            identity: NativeIdentity {
                id: self.name.clone(),
                qualifiers: [
                    ("native_object".into(), NATIVE_OBJECT.into()),
                    ("resource_pool_id".into(), self.pool_id.to_string()),
                ]
                .into(),
            },
            scope: MeasurementScope::Server,
            counters,
            // -1 bypasses admission. The factor is bytes for the small gate and a divisor for larger gates;
            // preserving the native policy avoids pretending that every factor is a memory allocation.
            properties: [
                ("threshold".into(), self.threshold.to_string()),
                ("threshold_factor".into(), self.threshold_factor.to_string()),
                ("is_active".into(), u8::from(self.enabled).to_string()),
            ]
            .into(),
        }
    }

    pub fn observation(&self) -> Result<OperationalObservation> {
        let counters = self.counters();
        Ok(OperationalObservation {
            source: crate::sources::COMPILATION_GATEWAYS.name.into(),
            key: counters.key()?,
            fact: OperationalFact::CounterSet(Box::new(counters)),
        })
    }

    fn waiting_signal(&self) -> Result<Option<Signal>> {
        if self.waiting == 0 {
            return Ok(None);
        }
        Ok(Some(Signal {
            measurement_scope: Some(MeasurementScope::Server),
            healthy: Some(culprit_core::t!(
                "provider.mssql.signal.compiler_gateway.context",
                active = culprit_core::format::count(self.active as f64),
                maximum = culprit_core::format::count(self.maximum as f64)
            )),
            ..Signal::new(
                &format!("compiler-gateway:{}", self.counters().key()?),
                culprit_core::message!(
                    "provider.mssql.signal.compiler_gateway",
                    gate = &self.name,
                    pool = self.pool_id.to_string()
                ),
                self.waiting as f64,
                SignalUnit::Count,
                SignalScope::Now,
            )
        }))
    }
}

pub(crate) async fn read<R: NativeRead>(reader: &mut R, limit: usize) -> Result<Vec<Gateway>> {
    let limit = limit.min(MAX_GATEWAYS);
    if limit == 0 {
        return Err(CollectionStop::RowLimit.error());
    }
    let rows = reader
        .query(SQL, vec![Parameter::Integer((limit + 1) as i64)], limit + 1)
        .await
        .map_err(ReadError::into_error)?;
    if rows.len() > limit {
        return Err(CollectionStop::RowLimit.error());
    }
    let samples = rows.iter().map(Gateway::decode).collect::<Result<Vec<_>>>()?;
    let mut identities = std::collections::BTreeSet::new();
    if samples.iter().any(|sample| !identities.insert((sample.pool_id, sample.name.as_str()))) {
        return Err(invalid_data());
    }
    Ok(samples)
}

pub(crate) async fn collect(
    pool: std::sync::Arc<crate::conn::Pool>,
    probe: &crate::probe::Probe,
    coverage: &mut Coverage,
) -> Vec<Signal> {
    if probe.major < 13 && !probe.is_azure_sql_database() {
        coverage.not_applicable(
            &crate::sources::COMPILATION_GATEWAYS,
            culprit_core::message!("provider.mssql.compiler_gateways.unavailable_version"),
        );
        return vec![];
    }
    let mut reader = ReadSession::new(pool, CollectionBudget::default());
    let result = read(&mut reader, MAX_GATEWAYS).await;
    let result = reader.finish(result).await.map_err(|error| {
        if error.code == culprit_core::ErrorCode::PermissionDenied {
            error.with_hint(probe.grant_hint())
        } else {
            error
        }
    });
    let Some(gateways) = coverage.read_rows(&crate::sources::COMPILATION_GATEWAYS, result) else { return vec![] };
    let signals = gateways.iter().filter_map(|sample| sample.waiting_signal().transpose()).collect::<Result<Vec<_>>>();
    match signals {
        Ok(signals) => signals,
        Err(error) => {
            coverage.source_failed(&crate::sources::COMPILATION_GATEWAYS, &error);
            vec![]
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
