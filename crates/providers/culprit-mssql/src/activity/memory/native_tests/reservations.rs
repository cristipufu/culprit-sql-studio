//! Both native cases use this one finite reservation-establishment path; failure never grows the workload.
use super::{
    CONTROL_TIMEOUT, FIXTURE_ROWS, SOURCE_QUERY_LIMIT, SOURCE_ROW_LIMIT, fixture::Fixture, observations::NativePoint,
};
use crate::{
    conn::{SessionKind, map_error},
    read::{Parameter, invalid_data, optional_counter, text},
    test_support::{assert_no_execution_grant, owned_case::until_cancelled, save_cost_receipt},
    value::Rec,
};
use culprit_core::{MemoryParticipantMechanism, Result, telemetry::*};
use futures_util::FutureExt;
use std::panic::AssertUnwindSafe;

#[derive(Clone, Copy)]
pub(super) enum HolderRequirement {
    Reservation,
    ClientOutputWait,
}

pub(super) struct ReservationPair {
    pub point: NativePoint,
    pub holder: ExecutionGrantObservation,
    pub waiter: ExecutionGrantObservation,
    pub waiter_index: usize,
}

pub(super) async fn establish(fixture: &mut Fixture, required: HolderRequirement) -> Result<ReservationPair> {
    fixture.start_actor().await?;
    while fixture.can_observe() {
        fixture.pause().await?;
        let point = fixture.observe().await?;
        if !fixture.outputs_are_held() {
            return Err(CollectionStop::SourceChanged.error());
        }
        let Some(holder) = point.actor(fixture.session(0)?)? else { continue };
        require_holder(holder)?;
        for waiter_index in 1..fixture.request_count() {
            let Some(waiter) = point.actor(fixture.session(waiter_index)?)? else { continue };
            if waiter.state == ExecutionGrantState::Waiting {
                require_pair(&point, holder, waiter)?;
                if matches!(required, HolderRequirement::ClientOutputWait) && !has_output_wait(&point, holder)? {
                    continue;
                }
                return Ok(ReservationPair { holder: holder.clone(), waiter: waiter.clone(), point, waiter_index });
            }
        }
        let session = fixture.session(fixture.request_count() - 1)?.clone();
        let Some(latest) = point.actor(&session)? else {
            // Numeric session equality is refusal context only. It can never associate an unresolved grant
            // with the held actor or repair its native request/session ordering.
            if point.observations.iter().any(|row| {
                row.request_association == RequestGrantAssociation::ChangedRequest
                    && row.identity.id.split_once(':').is_some_and(|(id, _)| id == session.id)
                    && row.requested_without_offset < session.generation
            }) {
                return Err(fixture.unexercised(
                    "native grant request time precedes the held login token; original association remains refused",
                ));
            }
            continue;
        };
        if latest.state == ExecutionGrantState::Granted {
            // Identical SQL may admit a third original actor only after actual native demand fits the hard cap.
            // No replacement is admitted for a missing/changed request or a native read failure.
            fixture.start_actor().await?;
        }
    }
    Err(fixture.observation_stop())
}

pub(super) fn has_output_wait(point: &NativePoint, holder: &ExecutionGrantObservation) -> Result<bool> {
    let key = holder.key()?;
    Ok(point.report.entries.iter().any(|entry| {
        entry.observation_key == key
            && entry.mechanism == Some(MemoryParticipantMechanism::ReservationDuringClientOutputWait)
    }))
}

fn require_holder(holder: &ExecutionGrantObservation) -> Result<()> {
    if holder.state != ExecutionGrantState::Granted
        || !holder.granted_kibibytes.as_deref().is_some_and(|value| value.parse::<u64>().is_ok_and(|amount| amount > 0))
    {
        return Err(invalid_data());
    }
    Ok(())
}

fn require_pair(
    point: &NativePoint,
    holder: &ExecutionGrantObservation,
    waiter: &ExecutionGrantObservation,
) -> Result<()> {
    let database = &point.context.target.database().ok_or_else(invalid_data)?.database;
    if waiter.state != ExecutionGrantState::Waiting
        || !matches!(waiter.granted_kibibytes.as_deref(), None | Some("0"))
        || !holder.shares_admission_resource(waiter, database)
        || point.queries != SOURCE_QUERY_LIMIT
    {
        return Err(invalid_data());
    }
    for row in [holder, waiter] {
        let request = row.request.as_ref().ok_or_else(invalid_data)?;
        if row.request_association != RequestGrantAssociation::GenerationChecked
            || row.statement_relationship != GrantStatementRelationship::ObservedCurrentStatement
            || request.key.session.generation.ends_with('Z')
            || request.key.request.as_ref().ok_or_else(invalid_data)?.generation.ends_with('Z')
            || !matches!(&request.statement, DefinitionPayload::Available { text, .. } if text.contains("request_memory_fixture"))
        {
            return Err(invalid_data());
        }
    }
    Ok(())
}

pub(super) async fn prove(fixture: &mut Fixture) -> Result<()> {
    let pair = establish(fixture, HolderRequirement::Reservation).await?;
    verify_metadata(fixture, &pair).await?;
    save_cost_receipt(
        &fixture.directory,
        "reservation-acceptance",
        &serde_json::json!({
            "original_point": pair.point.snapshot(),
            "holder": pair.holder,
            "waiter": pair.waiter,
            "same_native_resource": true,
            "metadata_plans_validated_without_execution_grants": true,
            "fixture_rows_per_actor": FIXTURE_ROWS,
            "native_cost_approval": false,
        }),
    )
}

async fn verify_metadata(fixture: &mut Fixture, pair: &ReservationPair) -> Result<()> {
    fixture.control_started()?;
    let pool = fixture.source_pool()?;
    let cancellation = fixture.cancellation.clone();
    let mut client =
        tokio::time::timeout(CONTROL_TIMEOUT, until_cancelled(&cancellation, pool.connect(SessionKind::User)))
            .await
            .map_err(|_| CollectionStop::CancellationUnconfirmed.error())??;
    let result = AssertUnwindSafe(until_cancelled(&cancellation, async {
        // These independent fixture plan probes preserve the old no-grant assertions. They are not source
        // acquisition queries, nor cost evidence for the boxed/MI/Azure production profiles.
        let inventory_plans =
            assert_no_execution_grant(&mut client, &crate::activity::memory::fields::inventory_sql(true), vec![])
                .await
                .len();
        let observation_plans = assert_no_execution_grant(
            &mut client,
            &pair.point.metadata.observation,
            vec![Parameter::Integer(i64::from(SOURCE_ROW_LIMIT) + 1)],
        )
        .await
        .len();
        let enrichment_plans = assert_no_execution_grant(
            &mut client,
            &crate::activity::sql::details(false, crate::activity::sql::QueryPolicy::MemoryDiagnostic),
            vec![Parameter::Text(pair.point.metadata.selectors.clone()), Parameter::Integer(16 * 1024)],
        )
        .await
        .len();
        let capacity = native_capacity(&mut client, &pair.holder).await?;
        compare_native_reservation(&mut client, &pair.holder, &pair.waiter).await?;
        Ok(serde_json::json!({
            "actual_plan_counts": [inventory_plans, observation_plans, enrichment_plans],
            "no_execution_grant_assertions_passed": true,
            "independent_later_native_semaphore": capacity,
            "source_query_accounting_includes_these_fixture_controls": false,
            "native_cost_approval": false,
        }))
    }))
    .catch_unwind()
    .await;
    match result {
        Ok(Ok(sanity)) => {
            tokio::time::timeout(CONTROL_TIMEOUT, client.close())
                .await
                .map_err(|_| CollectionStop::CancellationUnconfirmed.error())?
                .map_err(map_error)?;
            fixture.control_completed();
            save_cost_receipt(&fixture.directory, "metadata-and-native-sanity", &sanity)
        }
        Ok(Err(error)) => Err(error),
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

async fn compare_native_reservation(
    client: &mut crate::conn::MsClient,
    holder: &ExecutionGrantObservation,
    waiter: &ExecutionGrantObservation,
) -> Result<()> {
    let holder_key = &holder.request.as_ref().ok_or_else(invalid_data)?.key;
    let waiter_key = &waiter.request.as_ref().ok_or_else(invalid_data)?.key;
    let holder_id = &holder_key.session.id;
    let waiter_id = &waiter_key.session.id;
    let mut query = tiberius::Query::new(
        "SELECT TOP(3) /* culprit */ session_id,request_id,CONVERT(varchar(40),CAST(request_time AS datetime2),126) AS requested_without_offset,granted_memory_kb FROM sys.dm_exec_query_memory_grants WHERE (session_id=@P1 AND request_id=@P2) OR (session_id=@P3 AND request_id=@P4) OPTION(MAXDOP 1)",
    );
    query.bind(holder_id.parse::<i32>().map_err(|_| invalid_data())?);
    query.bind(holder_key.request.as_ref().ok_or_else(invalid_data)?.id.parse::<i32>().map_err(|_| invalid_data())?);
    query.bind(waiter_id.parse::<i32>().map_err(|_| invalid_data())?);
    query.bind(waiter_key.request.as_ref().ok_or_else(invalid_data)?.id.parse::<i32>().map_err(|_| invalid_data())?);
    let results = query.query(client).await.map_err(map_error)?.into_results().await.map_err(map_error)?;
    let [rows] = results.as_slice() else { return Err(invalid_data()) };
    if rows.len() != 2 {
        return Err(CollectionStop::SourceChanged.error());
    }
    let rows = rows.iter().map(Rec::from_row).collect::<Vec<_>>();
    for original in [holder, waiter] {
        let key = &original.request.as_ref().ok_or_else(invalid_data)?.key;
        let native = rows
            .iter()
            .find(|row| {
                row.string("session_id") == key.session.id
                    && row.string("request_id") == key.request.as_ref().unwrap().id
            })
            .ok_or_else(invalid_data)?;
        if crate::sql_time::native_time(&text(native, "requested_without_offset")?)?
            != original.requested_without_offset
        {
            return Err(CollectionStop::SourceChanged.error());
        }
    }
    let native_holder = rows.iter().find(|row| row.string("session_id") == *holder_id).ok_or_else(invalid_data)?;
    if holder.granted_kibibytes != optional_counter(native_holder, "granted_memory_kb")? {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(())
}

async fn native_capacity(
    client: &mut crate::conn::MsClient,
    holder: &ExecutionGrantObservation,
) -> Result<serde_json::Value> {
    let resource = holder.resource.as_ref().ok_or_else(invalid_data)?;
    let pool = resource.id.parse::<i32>().map_err(|_| invalid_data())?;
    let semaphore = resource
        .qualifiers
        .get("resource_semaphore_id")
        .ok_or_else(invalid_data)?
        .parse::<i32>()
        .map_err(|_| invalid_data())?;
    let mut query = tiberius::Query::new(
        "SELECT TOP(1) /* culprit */ target_memory_kb,available_memory_kb,granted_memory_kb,grantee_count,waiter_count FROM sys.dm_exec_query_resource_semaphores WHERE pool_id=@P1 AND resource_semaphore_id=@P2 OPTION(MAXDOP 1)",
    );
    query.bind(pool);
    query.bind(semaphore);
    let results = query.query(client).await.map_err(map_error)?.into_results().await.map_err(map_error)?;
    let [rows] = results.as_slice() else { return Err(invalid_data()) };
    let [row] = rows.as_slice() else { return Err(invalid_data()) };
    let row = Rec::from_row(row);
    Ok(serde_json::json!({
        "resource": resource,
        "target_memory_kibibytes": optional_counter(&row, "target_memory_kb")?,
        "available_memory_kibibytes": optional_counter(&row, "available_memory_kb")?,
        "granted_memory_kibibytes": optional_counter(&row, "granted_memory_kb")?,
        "grantee_count": optional_counter(&row, "grantee_count")?,
        "waiter_count": optional_counter(&row, "waiter_count")?,
        "same_clock_as_original_point": false,
    }))
}
