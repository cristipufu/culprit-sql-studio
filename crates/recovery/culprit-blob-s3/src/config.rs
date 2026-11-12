use culprit_collector::delivery::object_store::transport::valid_object_key as object_key;
use culprit_core::{Result, telemetry::DeliveryProblem};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct S3Config {
    pub bucket: String,
    pub region: String,
    pub prefix: String,
    /// An explicit S3-compatible origin. AWS regional endpoints are derived when this is absent.
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub expected_owner: Option<String>,
}
pub(super) struct Endpoint {
    pub origin: String,
    pub host: String,
    pub prefix: String,
    pub region: String,
    pub expected_owner: Option<String>,
}
impl S3Config {
    pub fn access_scope(&self) -> Result<String> {
        let endpoint = self.endpoint()?;
        Ok(format!("s3:{}/{}/{}", endpoint.origin, self.region, self.bucket))
    }
    pub(super) fn endpoint(&self) -> Result<Endpoint> {
        if !(3..=63).contains(&self.bucket.len())
            || !self.bucket.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || self.bucket.starts_with('-')
            || self.bucket.ends_with('-')
            || !culprit_core::aws_region_valid(&self.region)
            || !object_key(&self.prefix)
            || self.prefix.len() > 512
            || self
                .expected_owner
                .as_ref()
                .is_some_and(|owner| owner.len() != 12 || !owner.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(invalid());
        }
        let (origin, prefix) = if let Some(endpoint) = &self.endpoint {
            let uri: ureq::http::Uri = endpoint.parse().map_err(|_| invalid())?;
            let host = uri.host().ok_or_else(invalid)?;
            let loopback = host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
            if !matches!(uri.scheme_str(), Some("https")) && !(uri.scheme_str() == Some("http") && loopback) {
                return Err(invalid());
            }
            if uri.path() != "/" && !uri.path().is_empty()
                || uri.query().is_some()
                || endpoint.contains('@')
                || endpoint.contains('#')
            {
                return Err(invalid());
            }
            (endpoint.trim_end_matches('/').to_string(), format!("/{}/{}", self.bucket, self.prefix))
        } else {
            if self.expected_owner.is_none() {
                return Err(invalid());
            }
            let suffix = if self.region.starts_with("cn-") { "amazonaws.com.cn" } else { "amazonaws.com" };
            (format!("https://{}.s3.{}.{suffix}", self.bucket, self.region), format!("/{}", self.prefix))
        };
        let uri: ureq::http::Uri = origin.parse().map_err(|_| invalid())?;
        let host = uri.authority().ok_or_else(invalid)?.as_str().to_string();
        Ok(Endpoint { origin, host, prefix, region: self.region.clone(), expected_owner: self.expected_owner.clone() })
    }
}
impl Endpoint {
    pub fn path(&self, key: &str) -> Result<String> {
        if !object_key(key) {
            return Err(invalid());
        }
        Ok(format!("{}/{key}", self.prefix))
    }
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::InvalidContract.error()
}
