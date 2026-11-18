//! One nonce-owned database lifetime for native TVP cases, including unwind-safe cleanup.
use super::*;
use crate::test_support::{OwnedSourceFixture, verify_owned_source};

pub(super) async fn run(label: &str, work: impl AsyncFnOnce(&OwnedSourceFixture, &str)) {
    let fixture = Box::pin(OwnedSourceFixture::open("tvp-causal-fixture")).await.unwrap();
    let owner = std::env::var("CULPRIT_SMOKE_OWNER").unwrap();
    let database = format!(
        "culprit_tvp_{label}_{owner}_{}_{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap()
    );
    // The fixture retains this one body through unwind and native settlement without inlining its state.
    let body = AssertUnwindSafe(Box::pin(async {
        create_database(&fixture, &database).await;
        work(&fixture, &database).await;
    }))
    .catch_unwind()
    .await;
    // A separate owned actor closes the database after every body/setup failure. A successful Attention return
    // never allows a failed fixture actor to serve as cleanup authority.
    let cleanup = AssertUnwindSafe(Box::pin(delete_database(&fixture, &database))).catch_unwind().await;
    match (body, cleanup) {
        (Err(original), Err(_)) => {
            eprintln!("owned TVP cleanup also failed; no successful native qualification is recorded");
            std::panic::resume_unwind(original);
        }
        (Err(error), Ok(())) | (Ok(()), Err(error)) => std::panic::resume_unwind(error),
        (Ok(()), Ok(())) => {}
    }
}

async fn create_database(fixture: &OwnedSourceFixture, database: &str) {
    let mut setup = crate::test_support::connect(
        &config(fixture.port, "master", fixture.password(), "tvp-expression-setup"),
        SessionKind::User,
    )
    .await
    .unwrap();
    verify_owned_source(&mut setup).await.unwrap();
    let version = crate::conn::query_one(
        &mut setup,
        "SELECT /* culprit fixture */ CONVERT(int,SERVERPROPERTY('ProductMajorVersion')) AS major",
    )
    .await
    .unwrap();
    assert!(
        crate::read::signed_integer(&version, "major").unwrap() >= 16,
        "owned COUNT/TVP qualification requires SQL Server 2022+"
    );
    batch(
        &mut setup,
        &format!("CREATE DATABASE [{database}] COLLATE Latin1_General_100_CS_AS;ALTER DATABASE [{database}] SET COMPATIBILITY_LEVEL=160"),
    )
    .await;
    setup.close().await.unwrap();
}

async fn delete_database(fixture: &OwnedSourceFixture, database: &str) {
    let mut cleanup = crate::test_support::connect(
        &config(fixture.port, "master", fixture.password(), "tvp-expression-cleanup"),
        SessionKind::User,
    )
    .await
    .unwrap();
    verify_owned_source(&mut cleanup).await.unwrap();
    batch(
        &mut cleanup,
        &format!("IF DB_ID(N'{database}') IS NOT NULL BEGIN ALTER DATABASE [{database}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE;DROP DATABASE [{database}];END"),
    )
    .await;
    cleanup.close().await.unwrap();
}
