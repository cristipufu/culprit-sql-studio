use super::fixture::{Fixture, invalid};
use crate::{conn::map_error, value::Rec};
use culprit_core::Result;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Barrier;

pub(super) const WRITERS_PER_PATTERN: usize = 8;
const MAX_INSERTS: usize = 16_384;
const RUN_MS: u32 = 4000;

#[derive(Debug)]
pub(super) struct CompletedWorker {
    pub pattern: u8,
    pub inserts: u64,
}

impl Fixture {
    pub async fn start_workload(&mut self, split_second: bool, unrelated_select: bool) -> Result<Instant> {
        if !self.workers.is_empty() {
            return Err(invalid("previous fixture workers were not joined"));
        }
        let count = WRITERS_PER_PATTERN * 2 + usize::from(unrelated_select);
        let barrier = Arc::new(Barrier::new(count + 1));
        // Establish every connection before starting the comparable native work window.
        let mut clients = Vec::new();
        for index in 0..count {
            let pattern = if index == WRITERS_PER_PATTERN * 2 { 0 } else { (index % 2 + 1) as u8 };
            let application = match pattern {
                1 => "page-fixture-write-a",
                2 => "page-fixture-write-b",
                _ => "page-fixture-unrelated-select",
            };
            clients.push((self.client(application).await?, pattern));
        }
        for (mut client, pattern) in clients {
            let barrier = barrier.clone();
            let sql = statement(pattern, split_second);
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
        for slot in &mut self.workers {
            let mut task = slot.take().ok_or_else(|| invalid("owned worker was already joined"))?;
            match tokio::time::timeout(Duration::from_secs(10), &mut task).await {
                Ok(result) => completed.push(result.map_err(|_| invalid("owned worker task failed"))??),
                Err(_) => {
                    // Keep the pending owner for fixture cleanup; a completed handle is never polled twice.
                    *slot = Some(task);
                    return Err(invalid("owned worker exceeded finite workload deadline"));
                }
            }
        }
        self.workers.clear();
        Ok(completed)
    }
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
