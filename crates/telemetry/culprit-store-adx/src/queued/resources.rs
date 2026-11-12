//! Service-issued SAS locations and identity evidence stay in memory and expire as one bounded cache entry.
use culprit_core::{Result, telemetry::DeliveryProblem};
use serde_json::Value;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use tokio::time::Instant;
use zeroize::Zeroizing;

const MAX_RESOURCE_BYTES: usize = 64 * 1024;
const MAX_RESOURCES: usize = 128;
const MAX_CACHE_AGE: Duration = Duration::from_secs(5 * 60);
// Credentials must outlive upload and queued batching, not just submission to the queue.
const MIN_RESOURCE_LIFETIME_SECONDS: i64 = 15 * 60;

pub(super) struct Resources {
    pub containers: Vec<StorageResource>,
    pub queues: Vec<StorageResource>,
    pub identity: Zeroizing<String>,
    expires: Instant,
    valid: AtomicBool,
}
impl Resources {
    pub fn parse(locations: &[u8], identity: &[u8]) -> Result<Self> {
        let rows = management_rows(locations, &["ResourceTypeName", "StorageRoot"])?;
        let mut containers = Vec::new();
        let mut queues = Vec::new();
        for row in rows {
            let kind = row[0].as_str().ok_or_else(invalid)?;
            let (target, service) = match kind {
                "TempStorage" => (&mut containers, "blob"),
                "SecuredReadyForAggregationQueue" => (&mut queues, "queue"),
                _ => continue,
            };
            target.push(StorageResource::parse(row[1].as_str().ok_or_else(invalid)?, service)?);
        }
        if containers.is_empty() || queues.is_empty() {
            return Err(invalid());
        }
        let rows = management_rows(identity, &["AuthorizationContext"])?;
        if rows.len() != 1 {
            return Err(invalid());
        }
        let identity = rows[0][0].as_str().filter(|s| !s.is_empty() && s.len() <= 16384).ok_or_else(invalid)?;
        let remaining = containers
            .iter()
            .chain(&queues)
            .map(|resource| {
                resource.expires.signed_duration_since(chrono::Utc::now()).num_seconds() - MIN_RESOURCE_LIFETIME_SECONDS
            })
            .min()
            .ok_or_else(invalid)?;
        if remaining <= 0 {
            return Err(DeliveryProblem::Transport.error());
        }
        Ok(Self {
            containers,
            queues,
            identity: Zeroizing::new(identity.into()),
            expires: Instant::now() + MAX_CACHE_AGE.min(Duration::from_secs(remaining as u64)),
            valid: AtomicBool::new(true),
        })
    }
    pub fn usable(&self) -> bool {
        self.valid.load(Ordering::Acquire) && Instant::now() < self.expires
    }
    pub fn invalidate(&self) {
        self.valid.store(false, Ordering::Release);
    }
}

pub(super) struct StorageResource {
    pub origin: String,
    path: String,
    sas: Zeroizing<String>,
    expires: chrono::DateTime<chrono::Utc>,
}
impl StorageResource {
    fn parse(location: &str, service: &str) -> Result<Self> {
        if location.len() > 8192 || location.contains('#') {
            return Err(invalid());
        }
        let uri: ureq::http::Uri = location.parse().map_err(|_| invalid())?;
        let host = uri.host().ok_or_else(invalid)?;
        let suffix = format!(".{service}.core.windows.net");
        let account = host.strip_suffix(&suffix).ok_or_else(invalid)?;
        if uri.scheme_str() != Some("https")
            || uri.authority().map(|a| a.as_str()) != Some(host)
            || !(3..=24).contains(&account.len())
            || !account.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        {
            return Err(invalid());
        }
        let path = uri.path().trim_end_matches('/');
        let name = path.strip_prefix('/').ok_or_else(invalid)?;
        if !(3..=63).contains(&name.len())
            || name.starts_with('-')
            || name.ends_with('-')
            || name.contains("--")
            || !name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(invalid());
        }
        let sas = uri.query().ok_or_else(invalid)?;
        let mut expiry = None;
        let mut signature = false;
        for field in sas.split('&') {
            let (key, value) = field.split_once('=').ok_or_else(invalid)?;
            match key {
                "sig" => {
                    if signature || value.is_empty() {
                        return Err(invalid());
                    }
                    signature = true;
                }
                "se" => {
                    if expiry.is_some() {
                        return Err(invalid());
                    }
                    expiry = Some(
                        chrono::DateTime::parse_from_rfc3339(&decode_expiry(value)?)
                            .map_err(|_| invalid())?
                            .with_timezone(&chrono::Utc),
                    );
                }
                _ => (),
            }
        }
        if !signature {
            return Err(invalid());
        }
        Ok(Self {
            origin: format!("https://{host}"),
            path: path.into(),
            sas: Zeroizing::new(sas.into()),
            expires: expiry.ok_or_else(invalid)?,
        })
    }
    pub fn url(&self, suffix: &str) -> Zeroizing<String> {
        Zeroizing::new(format!("{}{}/{suffix}?{}", self.origin, self.path, self.sas.as_str()))
    }
}
fn decode_expiry(value: &str) -> Result<String> {
    if value.len() > 128 {
        return Err(invalid());
    }
    let mut decoded = Vec::new();
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        decoded.push(if byte == b'%' {
            let high = char::from(bytes.next().ok_or_else(invalid)?).to_digit(16).ok_or_else(invalid)?;
            let low = char::from(bytes.next().ok_or_else(invalid)?).to_digit(16).ok_or_else(invalid)?;
            (high * 16 + low) as u8
        } else {
            byte
        });
    }
    String::from_utf8(decoded).map_err(|_| invalid())
}
fn management_rows(bytes: &[u8], expected: &[&str]) -> Result<Vec<Vec<Value>>> {
    if bytes.len() > MAX_RESOURCE_BYTES {
        return Err(DeliveryProblem::Capacity.error());
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if value.get("error").is_some()
        || value.get("Exceptions").is_some_and(|e| e.as_array().is_none_or(|e| !e.is_empty()))
    {
        return Err(invalid());
    }
    let tables = value.get("Tables").and_then(Value::as_array).ok_or_else(invalid)?;
    let mut matching = tables.iter().filter(|table| {
        table.get("Columns").and_then(Value::as_array).is_some_and(|columns| {
            columns.len() == expected.len()
                && columns
                    .iter()
                    .zip(expected)
                    .all(|(c, name)| c["ColumnName"].as_str() == Some(name) && c["DataType"].as_str() == Some("String"))
        })
    });
    let table = matching.next().ok_or_else(invalid)?;
    if matching.next().is_some() {
        return Err(invalid());
    }
    let rows =
        table.get("Rows").and_then(Value::as_array).filter(|rows| rows.len() <= MAX_RESOURCES).ok_or_else(invalid)?;
    rows.iter()
        .map(|row| row.as_array().filter(|row| row.len() == expected.len()).cloned().ok_or_else(invalid))
        .collect()
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}
