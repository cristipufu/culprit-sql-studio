//! Stable, fixed-size updates keep a real owned target observable without holding or fabricating a latch.
use super::super::{
    fixture::{Fixture, actors, execute, invalid},
    workload::WRITERS_PER_PATTERN,
};
use crate::value::Rec;
use culprit_core::Result;
use std::{collections::BTreeMap, time::Instant};

const ACTORS: usize = WRITERS_PER_PATTERN * 2;
pub(super) const MAX_ITERATIONS: u64 = 16_384;
const RUN_MS: u32 = 4_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::activity::page_targets::native_tests) enum Role {
    Marker,
    Payload,
    TargetReader,
    UnrelatedReader,
    OtherTarget,
}

impl Role {
    pub(super) fn application(self) -> &'static str {
        match self {
            Self::Marker => "page-writer-marker-update",
            Self::Payload => "page-writer-payload-update",
            Self::TargetReader => "page-writer-target-reader",
            Self::UnrelatedReader => "page-writer-unrelated-reader",
            Self::OtherTarget => "page-writer-other-target",
        }
    }
    pub(super) fn from_application(application: &str) -> Option<Self> {
        [Self::Marker, Self::Payload, Self::TargetReader, Self::UnrelatedReader, Self::OtherTarget]
            .into_iter()
            .find(|role| role.application() == application)
    }
    pub(super) fn is_target_writer(self) -> bool {
        matches!(self, Self::Marker | Self::Payload)
    }
}

#[derive(Debug)]
pub(in crate::activity::page_targets::native_tests) enum CompletedWriter {
    /// Ordinary updates count matched rows; readers count completed SELECTs. Every error remains fatal.
    Work { role: Role, completed: u64 },
    /// Only the independently gated wrong-target control expects native lock timeout 1222.
    OtherTarget { updates: u64, lock_timeouts: u64, attempts: u64 },
}

pub(super) struct CompletionCounts {
    pub hot_updates: u64,
    pub other_updates: u64,
    pub other_lock_timeouts: u64,
    pub other_attempts: u64,
}

fn roles() -> Vec<Role> {
    let mut roles = vec![Role::Marker, Role::Payload, Role::TargetReader, Role::UnrelatedReader, Role::OtherTarget];
    roles.extend((roles.len()..ACTORS).map(|ordinal| if ordinal % 2 == 0 { Role::Marker } else { Role::Payload }));
    roles
}

impl Fixture {
    pub(super) async fn prepare_writer_pages(&mut self) -> Result<()> {
        self.require_idle_workload()?;
        self.reset().await?;
        let mut client = self.client("page-writer-setup").await?;
        // Each target updater owns a different fixed-size row. The independent wrong-target gate does not
        // lock these rows, and no target updater uses a long transaction or an imposed latch.
        let values = (1..ACTORS).map(|_| "(0)").collect::<Vec<_>>().join(",");
        execute(&mut client, &format!("INSERT dbo.page_fixture_hot(marker) VALUES {values}; CHECKPOINT;")).await
    }

    pub(super) async fn start_writer_workload(&mut self) -> Result<Instant> {
        self.require_idle_workload()?;
        let mut clients = Vec::new();
        // Control roles are connected first, so the existing bounded native-order roster can retain them.
        for (ordinal, role) in roles().into_iter().enumerate() {
            clients.push((self.client(role.application()).await?, role, statement(role, ordinal + 1)));
        }
        actors::start(&mut self.writer_workers, clients, decode_completion).await
    }

    pub(super) async fn finish_writer_workload(&mut self) -> Result<Vec<CompletedWriter>> {
        actors::finish(&mut self.writer_workers).await
    }

    pub(super) fn writer_workload_finished(&self) -> bool {
        self.writer_workers.iter().flatten().all(|worker| worker.is_finished())
    }
}

fn statement(role: Role, row: usize) -> String {
    let mut work = match role {
        Role::Marker => format!("UPDATE dbo.page_fixture_hot WITH(ROWLOCK) SET marker=marker^1 WHERE id={row} OPTION(MAXDOP 1); SET @i+=@@ROWCOUNT"),
        Role::Payload => format!("UPDATE dbo.page_fixture_hot WITH(ROWLOCK) SET payload=CASE WHEN payload=0x01 THEN 0x02 ELSE 0x01 END WHERE id={row} OPTION(MAXDOP 1); SET @i+=@@ROWCOUNT"),
        Role::OtherTarget => "UPDATE dbo.page_fixture_split WITH(ROWLOCK) SET payload=CONVERT(binary(16),HASHBYTES('SHA2_256',CONCAT(payload,marker))) WHERE id=1 OPTION(MAXDOP 1); SET @i+=@@ROWCOUNT".into(),
        Role::TargetReader => "SELECT @sink=SUM(CONVERT(bigint,CHECKSUM(HASHBYTES('SHA2_256',CONCAT(a.id,':',b.id))))) FROM dbo.page_fixture_hot a CROSS JOIN dbo.page_fixture_select b OPTION(MAXDOP 1); SET @i+=1".into(),
        Role::UnrelatedReader => "SELECT @sink=SUM(CONVERT(bigint,CHECKSUM(HASHBYTES('SHA2_256',CONCAT(a.id,':',b.id))))) FROM dbo.page_fixture_select a CROSS JOIN dbo.page_fixture_select b OPTION(MAXDOP 1); SET @i+=1".into(),
    };
    let (declarations, count, projection) = if role == Role::OtherTarget {
        // This is one native request with a fixed statement locator, not a collector retry. Count every
        // attempt against the existing quota; the unchanged one-second timeout still bounds each wait.
        work = format!(
            "SET @attempts+=1; BEGIN TRY {work}; END TRY BEGIN CATCH IF ERROR_NUMBER()<>1222 BEGIN THROW; END; SET @lock_timeouts+=1; END CATCH"
        );
        (
            "@lock_timeouts bigint=0,@attempts bigint=0,",
            "@attempts",
            "@i AS completed,@lock_timeouts AS lock_timeouts,@attempts AS attempts",
        )
    } else {
        ("", "@i", "@i AS completed")
    };
    format!(
        "SET NOCOUNT ON; SET LOCK_TIMEOUT 1000; DECLARE @i bigint=0,@sink bigint,{declarations}@until datetime2=DATEADD(millisecond,{RUN_MS},SYSUTCDATETIME()); WHILE {count}<{MAX_ITERATIONS} AND SYSUTCDATETIME()<@until BEGIN {work}; END; SELECT {projection};"
    )
}

fn decode_completion(role: Role, row: &Rec) -> Result<CompletedWriter> {
    let expected: &[&str] =
        if role == Role::OtherTarget { &["completed", "lock_timeouts", "attempts"] } else { &["completed"] };
    if row.columns().count() != expected.len()
        || expected.iter().any(|expected| row.columns().filter(|(column, _)| column == expected).count() != 1)
    {
        return Err(invalid("native writer completion changed its role-specific counter population"));
    }
    let completed = actors::completion_count(row, "completed")?;
    if role == Role::OtherTarget {
        Ok(CompletedWriter::OtherTarget {
            updates: completed,
            lock_timeouts: actors::completion_count(row, "lock_timeouts")?,
            attempts: actors::completion_count(row, "attempts")?,
        })
    } else {
        Ok(CompletedWriter::Work { role, completed })
    }
}

pub(super) fn require_completions(completed: &[CompletedWriter]) -> Result<CompletionCounts> {
    let mut counts = BTreeMap::new();
    let mut total = CompletionCounts { hot_updates: 0, other_updates: 0, other_lock_timeouts: 0, other_attempts: 0 };
    for worker in completed {
        let role = match *worker {
            CompletedWriter::Work { role, completed } => {
                if role == Role::OtherTarget || !(1..=MAX_ITERATIONS).contains(&completed) {
                    return Err(invalid("owned writer actor did not complete positive bounded work"));
                }
                if role.is_target_writer() {
                    total.hot_updates += completed;
                }
                role
            }
            CompletedWriter::OtherTarget { updates, lock_timeouts, attempts } => {
                if !(1..=MAX_ITERATIONS).contains(&attempts) || updates.checked_add(lock_timeouts) != Some(attempts) {
                    return Err(invalid("owned wrong-target control lost its exact update/lock-timeout attempts"));
                }
                total.other_updates += updates;
                total.other_lock_timeouts += lock_timeouts;
                total.other_attempts += attempts;
                Role::OtherTarget
            }
        };
        *counts.entry(role).or_insert(0) += 1;
    }
    let mut expected = BTreeMap::new();
    for role in roles() {
        *expected.entry(role).or_insert(0) += 1;
    }
    if counts != expected {
        return Err(invalid("owned writer completion lost its exact original role population"));
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Val;

    fn complete() -> Vec<CompletedWriter> {
        roles()
            .into_iter()
            .map(|role| {
                if role == Role::OtherTarget {
                    CompletedWriter::OtherTarget { updates: 3, lock_timeouts: 0, attempts: 3 }
                } else {
                    CompletedWriter::Work { role, completed: 3 }
                }
            })
            .collect()
    }

    #[test]
    fn fixed_page_work_keeps_distinct_roles_row_counts_and_the_existing_actor_time_bound() {
        assert_eq!(roles().len(), ACTORS);
        for (ordinal, role) in roles().into_iter().enumerate() {
            let sql = statement(role, ordinal + 1);
            assert!(sql.contains("SET LOCK_TIMEOUT 1000;"));
            assert!(sql.contains("DATEADD(millisecond,4000,SYSUTCDATETIME())"));
            if role == Role::OtherTarget {
                assert!(sql.contains("WHILE @attempts<16384 AND SYSUTCDATETIME()<@until"));
                assert!(sql.contains("SET @attempts+=1; BEGIN TRY UPDATE dbo.page_fixture_split"));
                assert!(sql.contains("IF ERROR_NUMBER()<>1222 BEGIN THROW; END; SET @lock_timeouts+=1;"));
                assert!(sql.contains("SELECT @i AS completed,@lock_timeouts AS lock_timeouts,@attempts AS attempts;"));
            } else {
                assert!(sql.contains("WHILE @i<16384 AND SYSUTCDATETIME()<@until"));
                assert!(!sql.contains("TRY") && !sql.contains("CATCH"));
                assert!(!sql.contains("lock_timeouts"));
            }
            if role.is_target_writer() {
                assert!(sql.contains(&format!("WHERE id={} ", ordinal + 1)));
                assert!(sql.contains("SET @i+=@@ROWCOUNT"));
            }
            assert!(!sql.contains("WAITFOR"));
            assert!(!sql.contains("BEGIN TRANSACTION"));
        }
    }

    #[test]
    fn missing_duplicate_zero_or_unbounded_actor_completions_cannot_qualify_native_writers() {
        let counts = require_completions(&complete()).unwrap();
        assert_eq!((counts.hot_updates, counts.other_updates), (39, 3));
        let mut missing = complete();
        missing.pop();
        assert!(require_completions(&missing).is_err());
        let mut duplicated = complete();
        duplicated.push(CompletedWriter::Work { role: Role::Marker, completed: 3 });
        assert!(require_completions(&duplicated).is_err());
        for count in [0, MAX_ITERATIONS + 1] {
            let mut changed = complete();
            changed[0] = CompletedWriter::Work { role: Role::Marker, completed: count };
            assert!(require_completions(&changed).is_err());
        }
    }

    #[test]
    fn only_the_typed_wrong_target_control_can_complete_zero_updates_with_native_timeouts() {
        let other = roles().iter().position(|role| *role == Role::OtherTarget).unwrap();
        let mut completed = complete();
        completed[other] = CompletedWriter::OtherTarget { updates: 0, lock_timeouts: 2, attempts: 2 };
        let counts = require_completions(&completed).unwrap();
        assert_eq!(
            (counts.hot_updates, counts.other_updates, counts.other_lock_timeouts, counts.other_attempts),
            (39, 0, 2, 2)
        );
        completed[other] = CompletedWriter::OtherTarget { updates: 7, lock_timeouts: 1, attempts: 8 };
        let counts = require_completions(&completed).unwrap();
        assert_eq!((counts.other_updates, counts.other_lock_timeouts, counts.other_attempts), (7, 1, 8));
        for invalid in [
            CompletedWriter::OtherTarget { updates: 0, lock_timeouts: 0, attempts: 0 },
            CompletedWriter::OtherTarget { updates: 0, lock_timeouts: 0, attempts: 1 },
            CompletedWriter::OtherTarget { updates: 1, lock_timeouts: 2, attempts: 2 },
            CompletedWriter::OtherTarget {
                updates: 0,
                lock_timeouts: MAX_ITERATIONS + 1,
                attempts: MAX_ITERATIONS + 1,
            },
            CompletedWriter::OtherTarget { updates: u64::MAX, lock_timeouts: 1, attempts: 1 },
            CompletedWriter::Work { role: Role::OtherTarget, completed: 3 },
        ] {
            completed[other] = invalid;
            assert!(require_completions(&completed).is_err());
        }
    }

    #[test]
    fn native_completion_requires_exact_role_columns_and_nonnegative_integer_counts() {
        let row = |updates, timeouts, attempts| {
            Rec::of(&[("completed", updates), ("lock_timeouts", timeouts), ("attempts", attempts)])
        };
        assert!(matches!(
            decode_completion(Role::OtherTarget, &row(Val::Int(0), Val::Int(2), Val::Int(2))).unwrap(),
            CompletedWriter::OtherTarget { updates: 0, lock_timeouts: 2, attempts: 2 }
        ));
        assert!(decode_completion(Role::Marker, &row(Val::Int(0), Val::Int(2), Val::Int(2))).is_err());
        assert!(decode_completion(Role::OtherTarget, &Rec::of(&[("completed", Val::Int(3))])).is_err());
        assert!(
            decode_completion(
                Role::OtherTarget,
                &Rec::of(&[("completed", Val::Int(0)), ("lock_timeouts", Val::Int(1)), ("lock_timeouts", Val::Int(1))])
            )
            .is_err()
        );
        for bad in [Val::Null, Val::Int(-1), Val::Float(1.0), Val::Text("1".into())] {
            assert!(decode_completion(Role::OtherTarget, &row(bad.clone(), Val::Int(1), Val::Int(1))).is_err());
            assert!(decode_completion(Role::OtherTarget, &row(Val::Int(0), bad.clone(), Val::Int(1))).is_err());
            assert!(decode_completion(Role::OtherTarget, &row(Val::Int(0), Val::Int(1), bad)).is_err());
        }
    }
}
