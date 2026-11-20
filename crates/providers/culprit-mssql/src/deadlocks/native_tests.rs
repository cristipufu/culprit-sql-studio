//! Scan and collector acceptance use the same guarded disposable XE fixture; no production target is changed.
use super::*;
use crate::{
    conn::{execute, map_error},
    read::ReadSession,
};
use culprit_core::telemetry::{CollectionBudget, DeadlockEvent, SourceShutdown};
use system_health::tests::owned_fixture;

#[tokio::test]
#[ignore = "requires nonce-owned disposable SQL Server and private native receipts; unrelated-history cost remains unqualified"]
async fn retained_event_reads_are_measured_with_large_unrelated_event_history() {
    owned_fixture::run("unrelated-retained-history", true, |fixture| Box::pin(async move {
        let session=fixture.additional_session("unrelated_events")?;
        let path=format!("{}_unrelated",fixture.file_stem);
        let mut admin=fixture.admin().await?;
        execute(&mut admin,&format!("CREATE EVENT SESSION [{session}] ON SERVER ADD EVENT sqlserver.error_reported(WHERE(error_number=50000)) ADD TARGET package0.event_file(SET filename=N'{}.xel',max_file_size=(128),max_rollover_files=(4)) WITH(MAX_DISPATCH_LATENCY=1 SECONDS,MAX_MEMORY=65536 KB,EVENT_RETENTION_MODE=ALLOW_SINGLE_EVENT_LOSS);ALTER EVENT SESSION [{session}] ON SERVER STATE=START",owned_fixture::quote(&path))).await.map_err(map_error)?;
        let native_sql=crate::statement_origin::monitoring_batch(&native::read_sql(false));
        let mut expected=0_i64;
        for added in [0,1000,9000] {
            if added>0 {
                execute(&mut admin,&format!("DECLARE @i int=0,@message nvarchar(2048)=REPLICATE(N'x',1024);WHILE @i<{added} BEGIN BEGIN TRY THROW 50000,@message,1;END TRY BEGIN CATCH SET @i+=1;END CATCH END")).await.map_err(map_error)?;
            }
            execute(&mut admin,&format!("ALTER EVENT SESSION [{session}] ON SERVER STATE=STOP;ALTER EVENT SESSION [{session}] ON SERVER STATE=START")).await.map_err(map_error)?;
            let history=crate::conn::query_one(&mut admin,&format!("SELECT COUNT_BIG(*) AS events,COALESCE(SUM(CONVERT(bigint,DATALENGTH(event_data))),0) AS payload_bytes FROM sys.fn_xe_file_target_read_file(N'{}*.xel',NULL,NULL,NULL)",owned_fixture::quote(&path))).await.map_err(map_error)?;
            expected+=added;
            let events=crate::read::integer(&history,"events")?;
            assert!(events>=expected,"generated unrelated history must be present before measurement");
            let files=crate::conn::query(&mut admin,&format!("SELECT DISTINCT file_name FROM sys.fn_xe_file_target_read_file(N'{}*.xel',NULL,NULL,NULL)",owned_fixture::quote(&path))).await.map_err(map_error)?;
            let files=files.iter().map(|row|culprit_core::telemetry::digest(row.string("file_name").as_bytes())).collect::<std::collections::BTreeSet<_>>();
            let until=fixture.native_time().await?;
            let mut reader=ReadSession::new(fixture.admitted.pool.clone(),CollectionBudget::default());
            let started=std::time::Instant::now();
            let found=native::read(&mut reader,native::Origin::SystemHealth,&fixture.admitted.database,&fixture.from,&until,501,false).await;
            let client_ms=started.elapsed().as_millis();
            let shutdown=reader.close().await;
            assert_eq!(shutdown,SourceShutdown::Quiescent);
            assert_eq!(reader.queries(),3,"configured metadata, file read, final metadata");
            let found=found?;
            assert!(found.iter().all(|event|!files.contains(&event.file_id)),"unrelated event files cannot become deadlock evidence");
            let mut costs=tiberius::Query::new("SELECT TOP(1) qs.last_worker_time,qs.last_elapsed_time,qs.last_logical_reads,qs.last_physical_reads FROM sys.dm_exec_query_stats qs CROSS APPLY sys.dm_exec_sql_text(qs.sql_handle) st WHERE RIGHT(st.text,LEN(@P1))=@P1 ORDER BY qs.last_execution_time DESC");
            costs.bind(native_sql.as_str());
            let measured=costs.query(&mut admin).await.map_err(map_error)?.into_row().await.map_err(map_error)?.ok_or_else(invalid_data)?;
            eprintln!("Retained-event experiment (unqualified): events={events},payload_bytes={},client_ms={client_ms},server_worker_us={},server_elapsed_us={},logical_reads_pages={},physical_reads_pages={}",crate::read::integer(&history,"payload_bytes")?,measured.get::<i64,_>("last_worker_time").ok_or_else(invalid_data)?,measured.get::<i64,_>("last_elapsed_time").ok_or_else(invalid_data)?,measured.get::<i64,_>("last_logical_reads").ok_or_else(invalid_data)?,measured.get::<i64,_>("last_physical_reads").ok_or_else(invalid_data)?);
        }
        Ok(())
    })).await.unwrap();
}

#[tokio::test]
#[ignore = "requires nonce-owned disposable SQL Server and private native receipts; shared history has no database membership proof"]
async fn native_deadlock_graphs_keep_positions_and_share_the_interactive_parser() {
    owned_fixture::run("native-deadlock-parser-parity", true, |fixture| {
        Box::pin(async move {
            let until = fixture.native_time().await?;
            let mut reader = ReadSession::new(fixture.admitted.pool.clone(), CollectionBudget::default());
            let mut stage = "first";
            let observed: Result<_> = async {
                let events = native::read(
                    &mut reader,
                    native::Origin::SystemHealth,
                    &fixture.admitted.database,
                    &fixture.from,
                    &until,
                    100,
                    false,
                )
                .await?;
                stage = "repeat";
                let repeat = native::read(
                    &mut reader,
                    native::Origin::SystemHealth,
                    &fixture.admitted.database,
                    &fixture.from,
                    &until,
                    100,
                    false,
                )
                .await?;
                stage = "interactive";
                let interactive = recent(&mut reader, 1.0).await?;
                Ok((events, repeat, interactive))
            }
            .await;
            let shutdown = reader.close().await;
            if observed.is_err()
                || observed.as_ref().is_ok_and(|(_, _, report)| report.scope != Some(MeasurementScope::Server))
            {
                eprintln!(
                    "Native deadlock parity refused: stage={stage}, code={:?}, stop={:?}, queries={}, shutdown={shutdown:?}, interactive_scope={:?}",
                    observed.as_ref().err().map(|error| error.code),
                    observed.as_ref().err().map(culprit_core::telemetry::CollectionStop::from_error),
                    reader.queries(),
                    observed.as_ref().ok().map(|(_, _, report)| report.scope),
                );
            }
            assert_eq!(shutdown, SourceShutdown::Quiescent);
            let (events, repeat, interactive) = observed?;
            assert_eq!(
                events.iter().map(|event| &event.position).collect::<Vec<_>>(),
                repeat.iter().map(|event| &event.position).collect::<Vec<_>>()
            );
            let event = events.iter().find(|event| !event.graphs.is_empty()).ok_or_else(invalid_data)?;
            let graph = &event.graphs[0];
            assert_eq!(graph.processes.iter().filter(|process| process.victim).count(), 1);
            let mut exported = DeadlockEvent {
                position: event.position.clone(),
                file_id: event.file_id.clone(),
                file_offset: event.offset.clone(),
                graph_ordinal: 0,
                at: event.at.clone(),
                processes: graph.processes.clone(),
                resources: graph.resources.clone(),
                pattern: graph.pattern,
            };
            exported.redact(Engine::Mssql);
            assert!(exported.is_valid());
            assert!(exported.processes.iter().all(|process| process.login.is_none() && process.host.is_none()));
            assert_eq!(interactive.scope, Some(MeasurementScope::Server));
            assert!(interactive.deadlocks.iter().any(|known| known.id == graph.id));
            assert_eq!(reader.queries(), 10, "two verified file reads plus one complete interactive read");
            Ok(())
        })
    })
    .await
    .unwrap();
}
