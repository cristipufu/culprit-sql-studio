//! The same finite, previously observed PSP-eligible skew is reused by native population and causal controls.
//! Setup is owned application work, never source work or production cost qualification.
use super::batch;
use crate::{conn::MsClient, read::integer};

pub(crate) const ROWS: i64 = 1_048_576;
pub(crate) const COMMON_ROWS: i64 = 786_432;
pub(crate) const PAYLOAD_BYTES: i64 = 16;
const MAX_DATA_MIB: u64 = 128;
const MAX_LOG_MIB: u64 = 512;
pub(crate) const MARKER: &str = "native_payload_length";
pub(crate) const STATEMENT: &str = "SELECT SUM(CONVERT(bigint,LEN(payload))) AS native_payload_length FROM dbo.parameter_variant_rows WHERE bucket=@bucket";

pub(crate) struct Seed {
    pub object_id: String,
    pub client_elapsed_ms: f64,
}

pub(crate) async fn seed(
    client: &mut MsClient,
    cancellation: &culprit_collector::capture::CollectionCancellation,
) -> Seed {
    active(cancellation);
    bound_owned_files(client, cancellation).await;
    batch(client,"CREATE TABLE dbo.parameter_variant_rows(id int NOT NULL PRIMARY KEY,bucket int NOT NULL,payload char(16) NOT NULL)").await;
    // Exact 8^6 * 4 native combinations, not an unbounded catalog/generator scan. No adaptive growth/retry.
    active(cancellation);
    batch(client,r#"CREATE PROCEDURE dbo.parameter_variant_seed AS BEGIN
WITH eight(n) AS(SELECT n FROM(VALUES(0),(1),(2),(3),(4),(5),(6),(7))v(n)),four(n) AS(SELECT n FROM(VALUES(0),(1),(2),(3))v(n))
INSERT dbo.parameter_variant_rows(id,bucket,payload)
SELECT 1+a.n+8*b.n+64*c.n+512*d.n+4096*e.n+16384*f.n+131072*g.n,CASE WHEN a.n+8*b.n+64*c.n+512*d.n+4096*e.n+16384*f.n+131072*g.n<786432 THEN 1 ELSE 2+(a.n+8*b.n+64*c.n+512*d.n+4096*e.n+16384*f.n+131072*g.n)%262144 END,
 CONVERT(char(16),CONCAT('native-',a.n,'-',b.n,'-',c.n,'-',d.n,'-',e.n))
FROM eight a CROSS JOIN eight b CROSS JOIN eight c CROSS JOIN eight d CROSS JOIN four e CROSS JOIN eight f CROSS JOIN eight g;
CREATE INDEX parameter_variant_bucket ON dbo.parameter_variant_rows(bucket);
UPDATE STATISTICS dbo.parameter_variant_rows WITH FULLSCAN;END"#).await;
    active(cancellation);
    let object = crate::conn::query_one(
        client,
        "SELECT /* culprit fixture */ OBJECT_ID(N'dbo.parameter_variant_seed') AS object_id",
    )
    .await
    .unwrap();
    active(cancellation);
    let started = std::time::Instant::now();
    batch(client, "EXEC dbo.parameter_variant_seed").await;
    let client_elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    active(cancellation);
    let storage = crate::conn::query_one(client,"SELECT /* culprit fixture */ SUM(CASE WHEN index_id IN(0,1) THEN row_count ELSE 0 END) AS rows,SUM(used_page_count) AS used_pages FROM sys.dm_db_partition_stats WHERE object_id=OBJECT_ID(N'dbo.parameter_variant_rows')").await.unwrap();
    assert_eq!(integer(&storage, "rows").unwrap(), ROWS);
    let used_pages =
        u64::try_from(integer(&storage, "used_pages").unwrap()).expect("native used pages must be nonnegative");
    // This is the whole owned table+index footprint, not a per-query grant/spill or log estimate.
    const MAX_TABLE_INDEX_PAGES: u64 = 16_384;
    assert!(used_pages <= MAX_TABLE_INDEX_PAGES, "owned table and index must remain within128MiB");
    eprintln!("owned PSP seed rows={ROWS} table/index native8KiB pages={used_pages} client_ms={client_elapsed_ms}");
    Seed { object_id: integer(&object, "object_id").unwrap().to_string(), client_elapsed_ms }
}

pub(crate) fn expected_result(bucket: i32) -> i64 {
    match bucket {
        1 => COMMON_ROWS * PAYLOAD_BYTES,
        2 | 3 => PAYLOAD_BYTES,
        _ => panic!("only the three fixed owned input classes are admitted"),
    }
}

#[test]
fn finite_psp_population_has_two_distinct_demands_and_equal_semantics_for_rare_controls() {
    assert_eq!(ROWS, 1_048_576);
    assert_eq!(COMMON_ROWS, ROWS / 4 * 3);
    assert_eq!(expected_result(1), 12_582_912);
    assert_eq!(expected_result(2), 16);
    assert_eq!(expected_result(2), expected_result(3));
}

fn active(cancellation: &culprit_collector::capture::CollectionCancellation) {
    assert!(!cancellation.is_cancelled(), "owned PSP setup cancelled before next statement");
}

/// This helper is called only after nonce admission and creation of a new disposable database. Exact native file
/// names remain quoted. Hard native caps prevent a test seed from silently growing data/log beyond its policy.
async fn bound_owned_files(client: &mut MsClient, cancellation: &culprit_collector::capture::CollectionCancellation) {
    let files=crate::conn::query(client,"SELECT TOP(3) /* culprit fixture */ DB_NAME() AS database_name,name,type,CONVERT(bigint,size) AS pages FROM sys.database_files ORDER BY file_id").await.unwrap();
    assert_eq!(files.len(), 2, "the disposable seed requires one native ROWS and one LOG file");
    let mut kinds = std::collections::BTreeSet::new();
    let files = files
        .iter()
        .map(|file| {
            let kind = integer(file, "type").unwrap();
            assert!(kinds.insert(kind));
            let maximum = match kind {
                0 => MAX_DATA_MIB,
                1 => MAX_LOG_MIB,
                _ => panic!("unexpected file in owned fixture"),
            };
            let pages = u64::try_from(integer(file, "pages").unwrap()).expect("native file pages must be nonnegative");
            assert!(pages <= maximum * 128);
            (crate::read::text(file, "database_name").unwrap(), crate::read::text(file, "name").unwrap(), maximum)
        })
        .collect::<Vec<_>>();
    for (database, name, maximum) in files {
        active(cancellation);
        batch(
            client,
            &format!(
                "ALTER DATABASE [{}] MODIFY FILE(NAME=N'{}',MAXSIZE={maximum}MB)",
                database.replace(']', "]]"),
                name.replace('\'', "''")
            ),
        )
        .await;
    }
}
