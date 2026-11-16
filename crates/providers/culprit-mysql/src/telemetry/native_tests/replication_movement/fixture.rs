use super::*;
use crate::{
    connect::Scope,
    read::{QueryRead, ReadSession},
};
use culprit_core::{ConnectionSpec, Engine};
use mysql_async::{Conn, prelude::Queryable};

pub(super) struct Topology {
    source: ConnectionSpec,
    replica: ConnectionSpec,
    source_owner: String,
    replica_owner: String,
    pub channel: String,
    pub table: String,
    pub source_host: String,
    pub source_uuid: String,
}

impl Topology {
    pub fn load() -> Self {
        let source_owner = environment("SOURCE_OWNER");
        let replica_owner = environment("REPLICA_OWNER");
        assert!(valid_owner(&source_owner) && valid_owner(&replica_owner) && source_owner != replica_owner);
        let channel = environment("CHANNEL");
        let table = environment("TABLE");
        assert_eq!(channel, format!("culprit_{source_owner}_moving"));
        assert_eq!(table, format!("culprit_moving_{source_owner}"));
        let source_host = environment("SOURCE_HOST");
        assert!(source_host.starts_with("culprit-smoke-") && source_host.ends_with(&format!("-{source_owner}")));
        let source_uuid = environment("SOURCE_UUID");
        required(crate::native::instance::resource(Flavor::Mysql, &source_uuid), "native source UUID");
        let source = specification("SOURCE_PORT", "root".into(), environment("SOURCE_PASSWORD"));
        let replica = specification("REPLICA_PORT", environment("MONITOR_USER"), environment("MONITOR_PASSWORD"));
        assert_ne!(source.port, replica.port, "the topology has two independent loopback endpoints");
        Self { source, replica, source_owner, replica_owner, channel, table, source_host, source_uuid }
    }

    pub fn replica(&self) -> crate::MysqlProvider {
        required(crate::MysqlProvider::new(&self.replica), "moving replica provider")
    }

    pub async fn guard(&self) -> (Conn, String) {
        let source = required(crate::MysqlProvider::new(&self.source), "moving source provider");
        let mut client = required(source.connector.connect_unconfigured().await, "owned source connection");
        let source_native = tokio::time::timeout(
            Duration::from_secs(5),
            client.query_first::<(String, u64, String, String), _>(
                "/* culprit */ SELECT @@hostname,@@GLOBAL.server_id,@@GLOBAL.server_uuid,VERSION()",
            ),
        )
        .await
        .expect("bounded native source ownership read")
        .map_err(crate::connect::map_error);
        let replica = self.replica();
        let mut reader = ReadSession::new(replica.connector.clone(), budget(), Scope::Server);
        let replica_native = reader.read(
            "SELECT @@hostname AS hostname,@@GLOBAL.server_id AS server_id,@@GLOBAL.server_uuid AS server_uuid,VERSION() AS version",
            vec![], 1,
        ).await;
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
        let source_native = required(source_native, "native source ownership").expect("one source identity");
        let replica_rows = required(replica_native, "native replica ownership");
        assert_eq!(replica_rows.len(), 1);
        let replica_native = &replica_rows[0];
        assert_eq!(source_native.0, format!("culprit-smoke-mysql-{}", self.source_owner));
        assert_eq!(source_native.1, 101);
        assert_eq!(source_native.2, self.source_uuid);
        assert!(source_native.3.starts_with("8.4."), "only the verified MySQL8.4 native dialect is admitted");
        assert_eq!(
            required(replica_native.native_text("hostname"), "native replica hostname").as_deref(),
            Some(format!("culprit-smoke-mysql-{}", self.replica_owner).as_str())
        );
        assert_eq!(required(replica_native.unsigned("server_id"), "native replica server ID"), Some(102));
        let replica_uuid =
            required(replica_native.native_text("server_uuid"), "native replica UUID").expect("native replica UUID");
        required(crate::native::instance::resource(Flavor::Mysql, &replica_uuid), "native replica UUID shape");
        assert_ne!(replica_uuid, self.source_uuid);
        let version =
            required(replica_native.native_text("version"), "native replica version").expect("native version");
        assert!(version.starts_with("8.4."));
        // This same source connection remains held from its original native owner check through the bounded write.
        (client, version)
    }

    pub async fn write(&self, mut client: Conn) {
        let values = (1..=32).map(|id| format!("({id},{})", id * 7)).collect::<Vec<_>>().join(",");
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            client.query_drop(format!(
                "/* culprit */ INSERT INTO culprit_smoke.{} (id,value) VALUES {values}",
                self.table
            )),
        )
        .await;
        let shutdown = tokio::time::timeout(Duration::from_secs(3), client.disconnect()).await;
        required(
            shutdown.expect("owned write connection close deadline").map_err(crate::connect::map_error),
            "owned write connection close",
        );
        required(
            result.expect("bounded owned fixture write deadline").map_err(crate::connect::map_error),
            "bounded owned fixture write",
        );
    }
}

fn specification(port: &str, user: String, password: String) -> ConnectionSpec {
    let port = environment(port).parse::<u16>().expect("owned moving fixture port");
    assert!(port > 0);
    ConnectionSpec {
        engine: Some(Engine::Mysql),
        host: "127.0.0.1".into(),
        port: Some(port),
        user: Some(user),
        password: Some(password),
        database: Some("culprit_smoke".into()),
        ..Default::default()
    }
}

fn environment(name: &str) -> String {
    std::env::var(format!("CULPRIT_MYSQL_MOVING_{name}"))
        .unwrap_or_else(|_| panic!("missing owned moving fixture {name}"))
}
