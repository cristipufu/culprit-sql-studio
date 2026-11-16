//! An explicit statement policy explains repeated compilation; it does not establish harmful reuse or pressure.
use culprit_core::{
    Engine,
    telemetry::{DefinitionPayload, MssqlQueryCompilation, StatementOrigin},
};

const MIN_RECORDED_COMPILATIONS: u64 = 2;
const MIN_AVERAGE_COMPILE_MICROSECONDS: f64 = 500_000.0;
const MIN_TOTAL_COMPILE_MILLISECONDS: f64 = 5_000.0;

#[derive(Clone, Copy)]
pub(crate) struct RepeatedRecompile<'a> {
    pub total_milliseconds: f64,
    pub statement: &'a str,
}

pub(crate) fn repeated_recompile(native: &MssqlQueryCompilation) -> Option<RepeatedRecompile<'_>> {
    let statement = recompile_statement(native)?;
    let count = native.compilations.parse::<u64>().ok()?;
    if count < MIN_RECORDED_COMPILATIONS || native.average_duration_us < MIN_AVERAGE_COMPILE_MICROSECONDS {
        return None;
    }
    let total_milliseconds = native.average_duration_us / 1_000.0 * count as f64;
    (total_milliseconds.is_finite() && total_milliseconds >= MIN_TOTAL_COMPILE_MILLISECONDS)
        .then_some(RepeatedRecompile { total_milliseconds, statement })
}

pub(super) fn recompile_statement(native: &MssqlQueryCompilation) -> Option<&str> {
    if !native.is_valid() || native.origin != StatementOrigin::Application {
        return None;
    }
    let DefinitionPayload::Available { text, .. } = &native.statement else { return None };
    if culprit_core::sql::single_statement(culprit_core::sql::without_parameter_list(text), Engine::Mssql).is_err()
        || !culprit_core::sql::has_mssql_recompile_hint(text)
    {
        return None;
    }
    Some(text)
}

#[cfg(test)]
pub(crate) mod tests;
