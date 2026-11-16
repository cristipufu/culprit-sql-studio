//! Read-only admission to an explicitly selected nonce-owned loopback smoke database.
use super::{config, connect};
use crate::{
    conn::{MsClient, Pool, SessionKind, map_error},
    read::{invalid_data, text},
    value::Rec,
};
use std::{sync::Arc, time::Instant};

pub(crate) struct OwnedSourceFixture {
    pub(crate) port: u16,
    pub(crate) database: String,
    pub(crate) pool: Arc<Pool>,
    pub(crate) admission_elapsed_ms: f64,
    password: String,
}

impl OwnedSourceFixture {
    pub(crate) async fn open(application: &str) -> culprit_core::Result<Self> {
        let started = Instant::now();
        let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").expect("owned loopback port").parse().unwrap();
        let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").expect("owned source password");
        let database = std::env::var("CULPRIT_SMOKE_DATABASE").expect("explicit owned smoke database");
        let mut admission =
            connect(&config(port, "master", &password, "cost-fixture-admission"), SessionKind::User).await?;
        verify_owned_source(&mut admission).await?;
        Ok(Self {
            pool: Arc::new(Pool::new(config(port, &database, &password, application), None)),
            port,
            database,
            password,
            admission_elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        })
    }

    pub(crate) fn password(&self) -> &str {
        &self.password
    }
}

pub(crate) async fn verify_owned_source(client: &mut MsClient) -> culprit_core::Result<()> {
    let owner = std::env::var("CULPRIT_SMOKE_OWNER").expect("explicit smoke owner nonce");
    if owner.len() != 24 || !owner.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid_data());
    }
    let rows = client
        .simple_query(
            "SELECT /* culprit fixture guard */ CONVERT(nvarchar(128),SERVERPROPERTY('MachineName')) AS machine",
        )
        .await
        .map_err(map_error)?
        .into_results()
        .await
        .map_err(map_error)?;
    let [rows] = rows.as_slice() else { return Err(invalid_data()) };
    let [row] = rows.as_slice() else { return Err(invalid_data()) };
    let machine = text(&Rec::from_row(row), "machine")?;
    if machine != format!("culprit-smoke-mssql-{owner}") {
        return Err(invalid_data());
    }
    Ok(())
}
