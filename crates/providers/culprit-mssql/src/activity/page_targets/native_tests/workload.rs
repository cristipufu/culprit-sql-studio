use super::fixture::{Fixture, actors, invalid};
use culprit_core::Result;
use std::time::Instant;

pub(super) const WRITERS_PER_PATTERN: usize = 8;
pub(super) const ABBA_INSERTS_PER_WORKER: usize = 2048;
pub(super) const HOLDER_INSERTS_PER_WORKER: usize = ABBA_INSERTS_PER_WORKER;
pub(super) const WRITER_APPLICATIONS: [&str; 2] = ["page-fixture-write-a", "page-fixture-write-b"];
const HOLDER_LOCK_TIMEOUT_MS: u32 = 5000;
const MAX_INSERTS: usize = 16_384;
const _: () = assert!(ABBA_INSERTS_PER_WORKER <= MAX_INSERTS);
const RUN_MS: u32 = 4000;

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
        self.start_workload_with_limit(split_second, unrelated_select, MAX_INSERTS).await
    }

    pub async fn start_abba_workload(&mut self, split_second: bool) -> Result<Instant> {
        self.start_workload_with_limit(split_second, false, ABBA_INSERTS_PER_WORKER).await
    }

    async fn start_workload_with_limit(
        &mut self,
        split_second: bool,
        unrelated_select: bool,
        inserts_per_worker: usize,
    ) -> Result<Instant> {
        let count = WRITERS_PER_PATTERN * 2 + usize::from(unrelated_select);
        let workers = (0..count)
            .map(|index| {
                let pattern = if index == WRITERS_PER_PATTERN * 2 { 0 } else { (index % 2 + 1) as u8 };
                WorkerStatement { pattern, sql: statement(pattern, split_second, inserts_per_worker) }
            })
            .collect();
        self.start_workers(workers).await
    }

    /// One bounded batch repeats its gated one-row INSERT at the same native statement locator. Each native
    /// request/token check still applies; this neither forces a holder nor changes the independent A/B workload.
    pub async fn start_holder_workload(&mut self) -> Result<Instant> {
        if !self.holder_gate_active() {
            return Err(invalid("owned repeated-INSERT actors require their retained seed gate"));
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
        self.require_idle_workload()?;
        // Establish every connection before starting the comparable native work window.
        let mut clients = Vec::new();
        for worker in workers {
            let application = match worker.pattern {
                1 => WRITER_APPLICATIONS[0],
                2 => WRITER_APPLICATIONS[1],
                _ => "page-fixture-unrelated-select",
            };
            clients.push((self.client(application).await?, worker.pattern, worker.sql));
        }
        actors::start(&mut self.workers, clients, |pattern, row| {
            Ok(CompletedWorker { pattern, inserts: actors::completion_count(row, "completed")? })
        })
        .await
    }
    pub fn workers_finished(&self) -> bool {
        self.workers.iter().flatten().all(|task| task.is_finished())
    }
    pub async fn finish_workload(&mut self) -> Result<Vec<CompletedWorker>> {
        actors::finish(&mut self.workers).await
    }
}

fn holder_statement(pattern: u8) -> String {
    // The seed gate is inside the actual repeated INSERT, so original capture does not select a separate
    // gate statement. Count only inserted rows, bounded by both the existing native deadline and actor quota.
    format!(
        "SET NOCOUNT ON; SET LOCK_TIMEOUT {HOLDER_LOCK_TIMEOUT_MS}; DECLARE @i int=0,@until datetime2=DATEADD(millisecond,{RUN_MS},SYSUTCDATETIME()); WHILE @i<{HOLDER_INSERTS_PER_WORKER} AND SYSUTCDATETIME()<@until BEGIN INSERT dbo.page_fixture_hot(marker) SELECT TOP (1) {pattern} FROM dbo.page_fixture_select WITH(READCOMMITTEDLOCK); SET @i+=@@ROWCOUNT; END; SELECT CONVERT(bigint,@i) AS completed;"
    )
}

pub(super) fn require_abba_completions(completed: &[CompletedWorker]) -> Result<()> {
    let mut counts = [0; WRITER_APPLICATIONS.len()];
    for worker in completed {
        let Some(count) = worker.pattern.checked_sub(1).and_then(|pattern| counts.get_mut(usize::from(pattern))) else {
            return Err(invalid("owned ABBA population included an unrelated worker"));
        };
        if worker.inserts != ABBA_INSERTS_PER_WORKER as u64 {
            return Err(invalid("owned ABBA actor did not complete its exact bounded insert quota"));
        }
        *count += 1;
    }
    if counts != [WRITERS_PER_PATTERN; WRITER_APPLICATIONS.len()] {
        return Err(invalid("owned ABBA population did not contain every original writer"));
    }
    Ok(())
}

fn statement(pattern: u8, split_second: bool, inserts_per_worker: usize) -> String {
    let work=match pattern {
        1=>"INSERT dbo.page_fixture_hot(marker) VALUES(1)".to_string(),
        2=>format!("INSERT dbo.{}(marker,payload) VALUES(2,0x02)",if split_second{"page_fixture_split"}else{"page_fixture_hot"}),
        _=>"SELECT @sink=SUM(CONVERT(bigint,CHECKSUM(HASHBYTES('SHA2_256',CONCAT(a.id,':',b.id))))) FROM dbo.page_fixture_select a CROSS JOIN dbo.page_fixture_select b OPTION(MAXDOP 1)".to_string(),
    };
    let limit = if pattern == 0 { 64 } else { inserts_per_worker };
    format!(
        "SET NOCOUNT ON; DECLARE @i int=0,@sink bigint,@until datetime2=DATEADD(millisecond,{RUN_MS},SYSUTCDATETIME()); WHILE @i<{limit} AND SYSUTCDATETIME()<@until BEGIN {work}; SET @i+=1; END; SELECT @i AS completed;"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete() -> Vec<CompletedWorker> {
        (0..WRITERS_PER_PATTERN * 2)
            .map(|index| CompletedWorker { pattern: (index % 2 + 1) as u8, inserts: ABBA_INSERTS_PER_WORKER as u64 })
            .collect()
    }

    #[test]
    fn abba_accepts_only_equal_complete_original_actor_populations() {
        require_abba_completions(&complete()).unwrap();
        let mut missing = complete();
        missing.pop();
        assert!(require_abba_completions(&missing).is_err());
        let mut repeated = complete();
        repeated.push(CompletedWorker { pattern: 1, inserts: ABBA_INSERTS_PER_WORKER as u64 });
        assert!(require_abba_completions(&repeated).is_err());
        for inserts in [0, ABBA_INSERTS_PER_WORKER as u64 - 1, ABBA_INSERTS_PER_WORKER as u64 + 1] {
            let mut rows = complete();
            rows[0].inserts = inserts;
            assert!(require_abba_completions(&rows).is_err());
        }
        for pattern in [0, 3] {
            let mut rows = complete();
            rows[0].pattern = pattern;
            assert!(require_abba_completions(&rows).is_err());
        }
    }

    #[test]
    fn holder_workload_repeats_one_gated_insert_with_exact_row_and_native_time_bounds() {
        for pattern in [1, 2] {
            let sql = holder_statement(pattern);
            assert!(sql.contains("WHILE @i<2048 AND SYSUTCDATETIME()<@until"));
            assert!(sql.contains("DATEADD(millisecond,4000,SYSUTCDATETIME())"));
            assert!(sql.contains("SET LOCK_TIMEOUT 5000"));
            assert!(sql.contains(&format!("SELECT TOP (1) {pattern} FROM dbo.page_fixture_select")));
            assert!(sql.contains("SELECT CONVERT(bigint,@i) AS completed"));
            assert_eq!(sql.matches("INSERT dbo.page_fixture_hot").count(), 1);
            let loop_start = sql.find("WHILE @i").unwrap();
            let insert = sql.find("INSERT dbo.page_fixture_hot").unwrap();
            let gate = sql.find("WITH(READCOMMITTEDLOCK)").unwrap();
            let counted = sql.find("SET @i+=@@ROWCOUNT").unwrap();
            assert!(loop_start < insert && insert < gate && gate < counted);
        }
    }

    #[test]
    fn equal_work_comparison_retains_the_original_native_deadline_and_insert_patterns() {
        for pattern in [1, 2] {
            for separated in [false, true] {
                let sql = statement(pattern, separated, ABBA_INSERTS_PER_WORKER);
                assert!(sql.contains("WHILE @i<2048 AND SYSUTCDATETIME()<@until"));
                assert!(sql.contains("DATEADD(millisecond,4000,SYSUTCDATETIME())"));
                let target = if pattern == 2 && separated { "page_fixture_split" } else { "page_fixture_hot" };
                assert!(sql.contains(&format!("INSERT dbo.{target}(")));
            }
        }
    }
}
