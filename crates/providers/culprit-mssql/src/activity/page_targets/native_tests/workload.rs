use super::fixture::{Fixture, invalid};
use crate::{conn::map_error, value::Rec};
use culprit_core::Result;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Barrier;

pub(super) const WRITERS_PER_PATTERN: usize = 8;
pub(super) const HOLDER_ROWS_PER_WORKER: usize = 8192;
pub(super) const WRITER_APPLICATIONS: [&str; 2] = ["page-fixture-write-a", "page-fixture-write-b"];
const HOLDER_LOCK_TIMEOUT_MS: u32 = 5000;
const MAX_INSERTS: usize = 16_384;
const RUN_MS: u32 = 4000;
const COMPLETION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub(super) struct CompletedWorker {
    pub pattern: u8,
    pub inserts: u64,
}

struct WorkerStatement {
    pattern: u8,
    sql: String,
}

impl Fixture {
    pub async fn start_workload(&mut self, split_second: bool, unrelated_select: bool) -> Result<Instant> {
        let count = WRITERS_PER_PATTERN * 2 + usize::from(unrelated_select);
        let workers = (0..count)
            .map(|index| {
                let pattern = if index == WRITERS_PER_PATTERN * 2 { 0 } else { (index % 2 + 1) as u8 };
                WorkerStatement { pattern, sql: statement(pattern, split_second) }
            })
            .collect();
        self.start_workers(workers).await
    }

    /// One bounded INSERT per actor keeps its native statement locator stable throughout the actual write.
    /// It does not force the engine to expose a holder or change the independent timed A/B workload.
    pub async fn start_holder_workload(&mut self) -> Result<Instant> {
        if !self.holder_gate_active() {
            return Err(invalid("owned single-INSERT actors require their retained seed gate"));
        }
        let workers = (0..WRITERS_PER_PATTERN * 2)
            .map(|index| {
                let pattern = (index % 2 + 1) as u8;
                WorkerStatement { pattern, sql: holder_statement(pattern) }
            })
            .collect();
        self.start_workers(workers).await
    }

    async fn start_workers(&mut self, workers: Vec<WorkerStatement>) -> Result<Instant> {
        if !self.workers.is_empty() {
            return Err(invalid("previous fixture workers were not joined"));
        }
        let barrier = Arc::new(Barrier::new(workers.len() + 1));
        // Establish every connection before starting the comparable native work window.
        let mut clients = Vec::new();
        for worker in workers {
            let application = match worker.pattern {
                1 => WRITER_APPLICATIONS[0],
                2 => WRITER_APPLICATIONS[1],
                _ => "page-fixture-unrelated-select",
            };
            clients.push((self.client(application).await?, worker));
        }
        for (mut client, WorkerStatement { pattern, sql }) in clients {
            let barrier = barrier.clone();
            self.workers.push(Some(tokio::spawn(async move {
                barrier.wait().await;
                let rows =
                    client.simple_query(sql).await.map_err(map_error)?.into_first_result().await.map_err(map_error)?;
                let row =
                    rows.first().map(Rec::from_row).ok_or_else(|| invalid("owned worker returned no completion"))?;
                let inserts = crate::read::integer(&row, "completed")?
                    .try_into()
                    .map_err(|_| invalid("invalid owned completion"))?;
                Ok(CompletedWorker { pattern, inserts })
            })));
        }
        let started = Instant::now();
        barrier.wait().await;
        Ok(started)
    }
    pub fn workers_finished(&self) -> bool {
        self.workers.iter().flatten().all(|task| task.is_finished())
    }
    pub async fn finish_workload(&mut self) -> Result<Vec<CompletedWorker>> {
        let mut completed = Vec::new();
        let deadline = tokio::time::Instant::now() + COMPLETION_TIMEOUT;
        for slot in &mut self.workers {
            let task = slot.as_mut().ok_or_else(|| invalid("owned worker was already joined"))?;
            let result = tokio::time::timeout_at(deadline, task)
                .await
                .map_err(|_| invalid("owned workers exceeded their shared completion deadline"))?;
            // Cancellation leaves the pending handle in its fixture; completed handles are removed before errors.
            slot.take();
            completed.push(result.map_err(|_| invalid("owned worker task failed"))??);
        }
        self.workers.clear();
        Ok(completed)
    }
}

fn holder_statement(pattern: u8) -> String {
    // Two fixed 100-row seed inputs bound the row population. The source-side locking hint only gates the
    // same INSERT for its original Activity capture; it does not identify or hold the target page's latch.
    format!(
        "SET NOCOUNT ON; SET LOCK_TIMEOUT {HOLDER_LOCK_TIMEOUT_MS}; INSERT dbo.page_fixture_hot(marker) SELECT TOP ({HOLDER_ROWS_PER_WORKER}) {pattern} FROM dbo.page_fixture_select a WITH(READCOMMITTEDLOCK) CROSS JOIN dbo.page_fixture_select b; SELECT CONVERT(bigint,@@ROWCOUNT) AS completed;"
    )
}

fn statement(pattern: u8, split_second: bool) -> String {
    let work=match pattern {
        1=>"INSERT dbo.page_fixture_hot(marker) VALUES(1)".to_string(),
        2=>format!("INSERT dbo.{}(marker,payload) VALUES(2,0x02)",if split_second{"page_fixture_split"}else{"page_fixture_hot"}),
        _=>"SELECT @sink=SUM(CONVERT(bigint,CHECKSUM(HASHBYTES('SHA2_256',CONCAT(a.id,':',b.id))))) FROM dbo.page_fixture_select a CROSS JOIN dbo.page_fixture_select b OPTION(MAXDOP 1)".to_string(),
    };
    let limit = if pattern == 0 { 64 } else { MAX_INSERTS };
    format!(
        "SET NOCOUNT ON; DECLARE @i int=0,@sink bigint,@until datetime2=DATEADD(millisecond,{RUN_MS},SYSUTCDATETIME()); WHILE @i<{limit} AND SYSUTCDATETIME()<@until BEGIN {work}; SET @i+=1; END; SELECT @i AS completed;"
    )
}
