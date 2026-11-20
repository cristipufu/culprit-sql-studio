//! One held fixture reader guards every command with the exact native nonce/database/session owner.
use super::*;
use crate::{connect::Flavor, rows::Rec};
use culprit_core::{ConnectionSpec, Engine};

// Eight owned database-session setup statements plus seventeen guarded fixture/control phases.
pub(super) const MAX_QUERIES: u32 = 25;
const CONTROL_STATEMENT_MS: u32 = 2_000;
const CONTROL_TOTAL_MS: u32 = 90_000;
const OWNERSHIP: &str = "SELECT /* culprit fixture */ @@hostname AS hostname,DATABASE() AS database_name,CONNECTION_ID() AS connection_id,@@GLOBAL.read_only AS read_only,VERSION() AS version,@@GLOBAL.event_scheduler AS scheduler_setting";

pub(super) fn specification() -> (ConnectionSpec, String) {
    let owner = std::env::var("CULPRIT_SMOKE_OWNER").expect("nonce-owned smoke supervisor");
    assert!(super::super::replication::valid_owner(&owner));
    assert_eq!(std::env::var("CULPRIT_SMOKE_ENGINE").as_deref(), Ok("mysql"));
    assert_eq!(std::env::var("CULPRIT_SMOKE_DATABASE").as_deref(), Ok("culprit_smoke"));
    let port: u16 = std::env::var("CULPRIT_MYSQL_TEST_PORT").expect("owned loopback port").parse().unwrap();
    assert!(port > 0);
    (
        ConnectionSpec {
            engine: Some(Engine::Mysql),
            host: "127.0.0.1".into(),
            port: Some(port),
            database: Some("culprit_smoke".into()),
            user: Some(std::env::var("CULPRIT_MYSQL_TEST_USER").unwrap_or_else(|_| "root".into())),
            password: Some(std::env::var("CULPRIT_MYSQL_TEST_PASSWORD").expect("owned root secret")),
            ..Default::default()
        },
        owner,
    )
}
pub(super) fn limited(original: &ConnectionSpec) -> ConnectionSpec {
    let mut specification = original.clone();
    specification.user = Some(std::env::var("CULPRIT_SMOKE_LIMITED_USER").expect("owned limited reader"));
    specification.password = Some(std::env::var("CULPRIT_SMOKE_LIMITED_PASSWORD").expect("owned limited secret"));
    specification
}
pub(super) fn budget() -> CollectionBudget {
    CollectionBudget {
        max_queries: MAX_QUERIES,
        max_snapshot_rows: 2,
        page_rows: 2,
        page_bytes: 8192,
        total_bytes: 64 * 1024,
        statement_timeout_ms: CONTROL_STATEMENT_MS,
        total_timeout_ms: CONTROL_TOTAL_MS,
        ..Default::default()
    }
}
#[derive(PartialEq, Eq)]
struct Owner {
    connection_id: u64,
    version: String,
    scheduler: MysqlEventSchedulerSetting,
}
fn decode_owner(row: &Rec, nonce: &str) -> Result<Owner> {
    let version = native::text(row, "version")?;
    if !super::super::replication::valid_owner(nonce)
        || native::text(row, "hostname")? != format!("culprit-smoke-mysql-{nonce}")
        || native::text(row, "database_name")? != "culprit_smoke"
        || native::integer(row, "read_only")? != 0
        || native::integer(row, "connection_id")? == 0
        || match Flavor::of_version(&version) {
            Flavor::Mysql => !culprit_core::version_at_least(&version, [5, 7, 0]),
            Flavor::Mariadb => !native::local_context::has_mariadb_uid(&version),
        }
    {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    let scheduler = match native::text(row, "scheduler_setting")?.as_str() {
        "ON" => MysqlEventSchedulerSetting::On,
        "OFF" => MysqlEventSchedulerSetting::Off,
        "DISABLED" => MysqlEventSchedulerSetting::Disabled,
        _ => return Err(CollectionStop::SourceUnavailable.error()),
    };
    Ok(Owner { connection_id: native::integer(row, "connection_id")?, version, scheduler })
}

pub(super) struct Control<R: OwnedRead = ReadSession> {
    pub(super) io: R,
    nonce: String,
    owner: Option<Owner>,
    cancellation: CollectionCancellation,
    can_dispatch: bool,
    created: bool,
}
impl<R: OwnedRead> Control<R> {
    pub(super) fn new(io: R, nonce: String, cancellation: CollectionCancellation) -> Self {
        Self { io, nonce, owner: None, cancellation, can_dispatch: true, created: false }
    }
    async fn dispatch(&mut self, sql: &str, parameters: Vec<Value>, limit: usize) -> Result<Vec<Rec>> {
        if !self.can_dispatch || self.cancellation.is_cancelled() {
            return Err(CollectionStop::Cancelled.error());
        }
        // Dropping this borrowed query leaves the same retained native reader to settle. It cannot admit cleanup.
        self.can_dispatch = false;
        let result = tokio::select! {
            biased;
            _ = self.cancellation.cancelled() => Err(CollectionStop::Cancelled.error()),
            result = self.io.read(sql, parameters, limit) => result,
        };
        self.can_dispatch = result.is_ok();
        result
    }
    async fn guard(&mut self) -> Result<()> {
        let rows = self.dispatch(OWNERSHIP, vec![], 1).await?;
        let [row] = rows.as_slice() else {
            self.can_dispatch = false;
            return Err(native::invalid());
        };
        let owner = match decode_owner(row, &self.nonce) {
            Ok(owner) => owner,
            Err(error) => {
                self.can_dispatch = false;
                return Err(error);
            }
        };
        if self.owner.as_ref().is_some_and(|original| original != &owner) {
            self.can_dispatch = false;
            return Err(CollectionStop::SourceChanged.error());
        }
        self.owner = Some(owner);
        Ok(())
    }
    async fn query(&mut self, sql: &str, parameters: Vec<Value>, limit: usize) -> Result<Vec<Rec>> {
        self.guard().await?;
        self.dispatch(sql, parameters, limit).await
    }
    pub(super) async fn admit(&mut self) -> Result<()> {
        budget().validate()?;
        let rows = self.query("SET SESSION TRANSACTION READ WRITE /* culprit fixture */", vec![], 1).await?;
        if !rows.is_empty() {
            return Err(native::invalid());
        }
        Ok(())
    }
    fn event_name(&self) -> String {
        format!("culprit_scheduled_{}", self.nonce)
    }
    pub(super) async fn create(&mut self) -> Result<()> {
        let name = self.event_name();
        let sql = format!(
            "CREATE DEFINER=CURRENT_USER EVENT `{name}` ON SCHEDULE AT CURRENT_TIMESTAMP + INTERVAL 1 DAY ON COMPLETION PRESERVE DISABLE COMMENT 'private_event_comment_{}' DO SET @culprit_private_event_body='private_event_body_{}' /* culprit fixture */",
            self.nonce, self.nonce
        );
        let rows = self.query(&sql, vec![], 1).await?;
        self.created = true;
        if !rows.is_empty() {
            return Err(native::invalid());
        }
        Ok(())
    }
    pub(super) async fn enable_future(&mut self) -> Result<()> {
        // The independently read schedule must remain over twelve hours in the future before enabling.
        self.native_event().await?.require_status("DISABLED")?;
        let rows =
            self.query(&format!("ALTER EVENT `{}` ENABLE /* culprit fixture */", self.event_name()), vec![], 1).await?;
        if !rows.is_empty() {
            return Err(native::invalid());
        }
        Ok(())
    }
    pub(super) async fn native_event(&mut self) -> Result<NativeEvent> {
        let rows = self.query(
            "SELECT /* culprit fixture */ EVENT_SCHEMA AS event_schema,EVENT_NAME AS event_name,STATUS AS status,EVENT_TYPE AS event_type,TIME_ZONE AS time_zone,DATE_FORMAT(EXECUTE_AT,'%Y-%m-%dT%H:%i:%s.%f') AS execute_at,DATE_FORMAT(CREATED,'%Y-%m-%dT%H:%i:%s.%f') AS created,DATE_FORMAT(LAST_ALTERED,'%Y-%m-%dT%H:%i:%s.%f') AS altered,LAST_EXECUTED AS last_started,DEFINER AS private_definer,EVENT_COMMENT AS private_comment,EVENT_DEFINITION AS private_body,(EXECUTE_AT>CURRENT_TIMESTAMP + INTERVAL 12 HOUR) AS safely_future FROM information_schema.EVENTS WHERE EVENT_SCHEMA=DATABASE() AND EVENT_NAME=? LIMIT 2",
            vec![Value::from(self.event_name())], 2,
        ).await?;
        let [row] = rows.as_slice() else {
            return Err(native::invalid());
        };
        let event = NativeEvent {
            schema: native::text(row, "event_schema")?,
            name: native::text(row, "event_name")?,
            status: native::text(row, "status")?,
            event_type: native::text(row, "event_type")?,
            time_zone: native::text(row, "time_zone")?,
            execute_at: native::text(row, "execute_at")?,
            created: native::text(row, "created")?,
            altered: native::text(row, "altered")?,
            definer: native::text(row, "private_definer")?,
            comment: native::text(row, "private_comment")?,
            body: native::text(row, "private_body")?,
            scheduler: self.owner.as_ref().ok_or_else(native::invalid)?.scheduler,
        };
        if event.schema != "culprit_smoke"
            || event.name != self.event_name()
            || event.event_type != "ONE TIME"
            || !native::flag(row, "safely_future")?
            || row.native_text("last_started")?.is_some()
            || event.comment != format!("private_event_comment_{}", self.nonce)
            || !event.body.contains(&format!("private_event_body_{}", self.nonce))
        {
            return Err(native::invalid());
        }
        Ok(event)
    }
    pub(super) async fn drop_event(&mut self) -> Result<()> {
        let rows = self.query(&format!("DROP EVENT `{}` /* culprit fixture */", self.event_name()), vec![], 1).await?;
        self.created = false;
        if !rows.is_empty() {
            return Err(native::invalid());
        }
        Ok(())
    }
    pub(super) async fn cleanup(&mut self) -> Result<()> {
        if self.created && self.can_dispatch && !self.cancellation.is_cancelled() {
            self.drop_event().await?;
        }
        Ok(())
    }
    pub(super) async fn close(&mut self) -> SourceShutdown {
        self.io.close().await
    }
    pub(super) fn requires_supervisor_destruction(&self) -> bool {
        self.created || !self.can_dispatch
    }
}
pub(super) struct NativeEvent {
    schema: String,
    name: String,
    status: String,
    event_type: String,
    time_zone: String,
    pub(super) execute_at: String,
    created: String,
    altered: String,
    definer: String,
    comment: String,
    body: String,
    pub(super) scheduler: MysqlEventSchedulerSetting,
}
impl NativeEvent {
    pub(super) fn require_status(&self, expected: &str) -> Result<()> {
        (self.status == expected).then_some(()).ok_or_else(native::invalid)
    }
    pub(super) fn assert_event(&self, event: &MysqlScheduledEvent, status: MysqlEventStatus) {
        assert_eq!(event.schema, self.schema);
        assert_eq!(event.name, self.name);
        assert_eq!(event.status, Some(status));
        assert_eq!(event.event_type, Some(MysqlEventType::OneTime));
        assert_eq!(event.time_zone.as_deref(), Some(self.time_zone.as_str()));
        assert_eq!(event.execute_at_without_offset.as_deref(), Some(self.execute_at.as_str()));
        assert_eq!(event.created_without_offset.as_deref(), Some(self.created.as_str()));
        assert_eq!(event.altered_without_offset.as_deref(), Some(self.altered.as_str()));
        assert_eq!(event.starts_without_offset, None);
        assert_eq!(event.ends_without_offset, None);
        assert_eq!(event.interval_value, None);
        assert_eq!(event.interval_field, None);
        assert_eq!(event.last_started_without_offset, None);
    }
    pub(super) fn assert_private(&self, captured: &capture::Capture) {
        let encoded = serde_json::to_string(&(&captured.manifest, &captured.records)).unwrap();
        for value in [&self.definer, &self.comment, &self.body] {
            assert!(!value.is_empty());
            assert!(!encoded.contains(value), "event private catalog body/definer/comment reached capture");
        }
        for field in ["private_definer", "private_comment", "private_body", "event_definition", "event_comment"] {
            assert!(!encoded.to_ascii_lowercase().contains(field), "private event field reached source export");
        }
    }
}
