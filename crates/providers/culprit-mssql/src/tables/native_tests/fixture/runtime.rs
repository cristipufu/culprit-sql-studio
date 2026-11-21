//! One finite local full-population start. Native runtime presence and status are observed independently.
use super::*;

pub(in crate::tables::native_tests) const SYNTHETIC_ROWS: u32 = 512;
const CHARACTERS_PER_ROW: u32 = 1000;
const SYNTHETIC_TEXT: &str = "culprit synthetic token ";
const REPETITIONS: u32 = 40;

fn seed_sql(table: &str) -> String {
    format!(
        "WITH digits(n) AS(SELECT n FROM(VALUES(0),(1),(2),(3),(4),(5),(6),(7),(8),(9))d(n)),\
         numbers AS(SELECT TOP({SYNTHETIC_ROWS}) ROW_NUMBER() OVER(ORDER BY a.n,b.n,c.n)n FROM digits a CROSS JOIN digits b CROSS JOIN digits c) \
         INSERT dbo.[{table}](id,content) SELECT CONVERT(int,n),REPLICATE(CONVERT(nvarchar(max),N'{SYNTHETIC_TEXT}'),{REPETITIONS}) FROM numbers;\
         SELECT /* culprit fixture seed count */ @@ROWCOUNT AS inserted_rows"
    )
}
impl Fixture<'_> {
    pub(in crate::tables::native_tests) async fn start_full_population(&mut self) -> Result<()> {
        assert_eq!(self.population_work, PopulationWork::NotStarted);
        let table = self.indexed_table();
        let seed = seed_sql(&table);
        let oracle = self.identity_sql();
        let original = self.population_target();
        // Mark possible background ownership BEFORE the dispatch that can start it. An error never resets it.
        self.population_work = PopulationWork::MayBeRunning;
        let ids = self
            .control("culprit-full-text-runtime-control", async |commands, _| {
                let inserted = commands.row(&seed).await;
                assert_eq!(crate::read::integer(&inserted, "inserted_rows").unwrap(), i64::from(SYNTHETIC_ROWS));
                commands.batch(&format!("ALTER FULLTEXT INDEX ON dbo.[{table}] START FULL POPULATION")).await;
                NativeIds::decode(&commands.row(&oracle).await)
            })
            .await?;
        assert_eq!(ids.database, original.table.database_id);
        assert_eq!(ids.indexed, original.table.object_id);
        assert_eq!(
            crate::sql_time::native_time(&ids.created_without_offset)?,
            crate::sql_time::native_time(&original.table.created_without_offset)?
        );
        assert_eq!(ids.catalog, Some(original.catalog_id));
        assert_eq!(ids.unique_index, original.unique_index_id);
        // START is fixture DDL: acquire the native modification point before the original capture,
        // never attach this later metadata retroactively to an earlier source observation.
        self.ids = Some(ids);
        Ok(())
    }
    pub(in crate::tables::native_tests) async fn stop_original_population(&mut self) {
        assert_eq!(self.population_work, PopulationWork::MayBeRunning);
        assert!(!self.population_capture_unsettled);
        let table = self.indexed_table();
        self.control("culprit-full-text-runtime-stop", async |commands, _| {
            commands.batch(&format!("ALTER FULLTEXT INDEX ON dbo.[{table}] STOP POPULATION")).await;
        })
        .await;
        self.population_work = PopulationWork::StopCommandCompleted;
    }
    pub(in crate::tables::native_tests) fn runtime_accounting(&self) -> serde_json::Value {
        serde_json::json!({
            "control":self.control_accounting(),
            "canonical_capture_attempts":self.population_capture_attempts,
            "canonical_captures_completed":self.population_captures_completed,
            "canonical_capture_cap":1,
            "canonical_source_queries_including_setup_cap":12,
            "combined_fixture_and_source_dispatch_cap_including_admission_and_setups":42,
            "synthetic_rows":SYNTHETIC_ROWS,
            "synthetic_characters_per_row_cap":CHARACTERS_PER_ROW,
            "synthetic_utf16_payload_bytes_cap":SYNTHETIC_ROWS*CHARACTERS_PER_ROW*2,
            "start_commands_cap":1,"pause_commands_cap":0,"stop_commands_cap":1,
            "native_background_state":format!("{:?}",self.population_work),
            "background_idle_proved":false,
            "cleanup_owner":"nonce-owned disposable-container supervisor",
            "native_cost_approved":false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_population_seed_is_finite_and_does_not_read_any_existing_table() {
        let sql = seed_sql("owned_fixture");
        assert!(sql.contains("TOP(512)"));
        assert!(sql.contains("@@ROWCOUNT AS inserted_rows"));
        assert!(!sql.contains("sys."));
        assert!(!sql.contains("WAITFOR"));
        assert!(!sql.contains("WHILE"));
        assert!(
            SYNTHETIC_TEXT.encode_utf16().count() as u32 * REPETITIONS <= CHARACTERS_PER_ROW,
            "fixed synthetic text must fit the declared row bound"
        );
    }
    #[test]
    fn a_stop_command_is_not_a_background_idle_or_cleanup_completion_proof() {
        assert!(!PopulationWork::NotStarted.requires_supervisor());
        assert!(PopulationWork::MayBeRunning.requires_supervisor());
        assert!(PopulationWork::StopCommandCompleted.requires_supervisor());
    }
}
