//! AWS Signature V4 for exact S3 object operations. No presigned URLs, redirects, unsigned payloads or automatic retries.
//! https://docs.aws.amazon.com/AmazonS3/latest/API/sig-v4-authenticating-requests.html
use crate::config::Endpoint;
use culprit_auth::AccessKeyCredential;
use culprit_core::{Result, telemetry::DeliveryProblem};
use std::collections::BTreeMap;
use zeroize::Zeroizing;

pub(super) struct SignedHeaders {
    pub headers: BTreeMap<String, Zeroizing<String>>,
}
pub(super) fn sign(
    method: &str,
    path: &str,
    endpoint: &Endpoint,
    credentials: &AccessKeyCredential,
    payload_sha256: &str,
    condition: Option<(&str, &str)>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<SignedHeaders> {
    if !safe(credentials.key_id.as_str())
        || !safe(credentials.secret_key.as_str())
        || credentials.session_token.as_ref().is_some_and(|token| !safe(token.as_str()))
        || credentials.expires_at.is_some_and(|expiry| expiry <= std::time::SystemTime::now())
    {
        return Err(DeliveryProblem::Transport.error());
    }
    let date = now.format("%Y%m%d").to_string();
    let timestamp = now.format("%Y%m%dT%H%M%SZ").to_string();
    let mut headers = BTreeMap::from([
        ("host".into(), Zeroizing::new(endpoint.host.clone())),
        ("x-amz-content-sha256".into(), Zeroizing::new(payload_sha256.into())),
        ("x-amz-date".into(), Zeroizing::new(timestamp.clone())),
    ]);
    if let Some((name, value)) = condition {
        headers.insert(name.into(), Zeroizing::new(value.into()));
    }
    if let Some(owner) = &endpoint.expected_owner {
        headers.insert("x-amz-expected-bucket-owner".into(), Zeroizing::new(owner.clone()));
    }
    if let Some(token) = &credentials.session_token {
        headers.insert("x-amz-security-token".into(), token.clone());
    }
    let names = headers.keys().cloned().collect::<Vec<_>>().join(";");
    let canonical_headers =
        Zeroizing::new(headers.iter().map(|(key, value)| format!("{key}:{}\n", value.as_str())).collect::<String>());
    let canonical =
        Zeroizing::new(format!("{method}\n{path}\n\n{}\n{names}\n{payload_sha256}", canonical_headers.as_str()));
    let scope = format!("{date}/{}/s3/aws4_request", endpoint.region);
    let to_sign = format!("AWS4-HMAC-SHA256\n{timestamp}\n{scope}\n{}", sha256(canonical.as_bytes()));
    let seed = Zeroizing::new(format!("AWS4{}", credentials.secret_key.as_str()));
    let date_key = hmac(seed.as_bytes(), date.as_bytes());
    let region_key = hmac(&date_key, endpoint.region.as_bytes());
    let service_key = hmac(&region_key, b"s3");
    let request_key = hmac(&service_key, b"aws4_request");
    let signature = hmac(&request_key, to_sign.as_bytes());
    headers.insert(
        "authorization".into(),
        Zeroizing::new(format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={names}, Signature={}",
            credentials.key_id.as_str(),
            hex(&signature)
        )),
    );
    Ok(SignedHeaders { headers })
}
pub(super) fn sha256(bytes: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}
pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn hmac(key: &[u8], bytes: &[u8]) -> Zeroizing<Vec<u8>> {
    Zeroizing::new(ring::hmac::sign(&ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key), bytes).as_ref().to_vec())
}
fn safe(value: &str) -> bool {
    !value.is_empty() && value.len() <= 16384 && value.bytes().all(|b| b.is_ascii_graphic())
}
