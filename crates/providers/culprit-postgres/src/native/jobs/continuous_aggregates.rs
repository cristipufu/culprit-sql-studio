//! Read only selected configuration from the qualified extension-owned public catalog.
use super::*;

pub(super) async fn read<R: QueryRead>(
    reader: &mut R,
    catalog: &Discovery,
    limit: u32,
    output: &mut Vec<OperationalObservation>,
) -> Result<StateSourceCoverage> {
    let Some(view) = catalog.available(TimescaleJobViewKind::ContinuousAggregates) else {
        return Ok(
            catalog.unavailable(TIMESCALE_CONTINUOUS_AGGREGATES_SOURCE, TimescaleJobViewKind::ContinuousAggregates)
        );
    };
    if limit == 0 {
        return Ok(exhausted(TIMESCALE_CONTINUOUS_AGGREGATES_SOURCE));
    }
    let mut rows = match reader.query(&sql(view, limit + 1), limit as usize + 1).await {
        Ok(rows) => rows,
        Err(error)
            if error.detail.is_none() && matches!(error.code, ErrorCode::PermissionDenied | ErrorCode::Unsupported) =>
        {
            return Ok(StateSourceCoverage::unavailable(
                TIMESCALE_CONTINUOUS_AGGREGATES_SOURCE.into(),
                error.code,
                None,
            ));
        }
        Err(error) => return Err(error),
    };
    let truncated = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    let points = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let point = decode(row, index as u32 + 1)?;
            Ok(OperationalObservation {
                source: TIMESCALE_CONTINUOUS_AGGREGATES_SOURCE.into(),
                key: point.key()?,
                fact: OperationalFact::TimescaleContinuousAggregate(Box::new(point)),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut coverage =
        StateSourceCoverage::observed(TIMESCALE_CONTINUOUS_AGGREGATES_SOURCE.into(), points.len() as u32, !truncated);
    if truncated {
        coverage.stopped = Some(CollectionStop::RowLimit);
    }
    output.extend(points);
    Ok(coverage)
}

fn sql(view: &discovery::View, limit: u32) -> String {
    let granular = if view.has("granular_refresh_enabled") { "c.granular_refresh_enabled::text" } else { "NULL::text" };
    format!(
        "WITH stamp AS (SELECT pg_catalog.clock_timestamp() AS at) SELECT {} AS observed_at,\
         c.hypertable_schema::text,c.hypertable_name::text,c.view_schema::text,c.view_name::text,\
         c.view_owner::text,c.materialized_only::text,c.compression_enabled::text,\
         c.materialization_hypertable_schema::text,c.materialization_hypertable_name::text,\
         {granular} AS granular_refresh_enabled FROM timescaledb_information.continuous_aggregates c CROSS JOIN stamp \
         ORDER BY c.view_schema,c.view_name LIMIT {limit}",
        iso("stamp.at"),
    )
}

fn relation(row: &Row, schema: &str, name: &str) -> Result<TimescaleRelationLabel> {
    Ok(TimescaleRelationLabel { schema: text(row, schema)?, name: text(row, name)? })
}
fn decode(row: &Row, position: u32) -> Result<TimescaleContinuousAggregate> {
    let point = TimescaleContinuousAggregate {
        position,
        observed_at: text(row, "observed_at")?,
        view: relation(row, "view_schema", "view_name")?,
        source_hypertable: relation(row, "hypertable_schema", "hypertable_name")?,
        materialization_hypertable: relation(
            row,
            "materialization_hypertable_schema",
            "materialization_hypertable_name",
        )?,
        owner: text(row, "view_owner")?,
        materialized_only: flag(row, "materialized_only")?,
        compression_enabled: flag(row, "compression_enabled")?,
        granular_refresh_enabled: match row.value("granular_refresh_enabled") {
            Some(None) => None,
            Some(Some(_)) => Some(flag(row, "granular_refresh_enabled")?),
            None => return Err(invalid()),
        },
    };
    point.is_valid().then_some(point).ok_or_else(invalid)
}

#[cfg(test)]
mod tests;
