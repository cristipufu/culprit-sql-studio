use super::*;
use std::{sync::OnceLock, time::Duration};

const METADATA_BYTES: u64 = 64 * 1024;
const HTTP_TIMEOUT: Duration = Duration::from_secs(45);

pub(super) fn discover(origin: &str) -> Result<Audience> {
    let mut response = agent(origin)
        .get(format!("{origin}/v1/rest/auth/metadata"))
        .header("Accept", "application/json")
        .header("Accept-Encoding", "identity")
        .call()
        .map_err(|_| DeliveryProblem::Transport.error())?;
    if response.status().as_u16() != 200 {
        return Err(DeliveryProblem::Transport.error());
    }
    let bytes = response
        .body_mut()
        .with_config()
        .limit(METADATA_BYTES)
        .read_to_vec()
        .map_err(|_| DeliveryProblem::Transport.error())?;
    audience(origin, &bytes)
}
pub(super) fn audience(origin: &str, bytes: &[u8]) -> Result<Audience> {
    if bytes.len() as u64 > METADATA_BYTES {
        return Err(DeliveryProblem::Capacity.error());
    }
    let metadata: Value = serde_json::from_slice(bytes).map_err(|_| DeliveryProblem::Integrity.error())?;
    let advertised = metadata
        .get("AzureAD")
        .and_then(|azure| azure.get("KustoServiceResourceId"))
        .and_then(Value::as_str)
        .ok_or_else(|| DeliveryProblem::InvalidContract.error())?;
    Audience::kusto(origin, advertised)
}
pub(super) fn query(
    origin: &str,
    token: &str,
    request_id: &str,
    body: Vec<u8>,
    columns: &[Column],
    row_limit: usize,
) -> Result<QueryTable> {
    let auth = Zeroizing::new(format!("Bearer {token}"));
    let mut response = agent(origin)
        .post(format!("{origin}/v2/rest/query"))
        .header("Accept", "application/json")
        .header("Content-Type", "application/json; charset=utf-8")
        .header("Accept-Encoding", "identity")
        .header("Authorization", auth.as_str())
        .header("x-ms-client-request-id", request_id)
        .header("x-ms-app", "Culprit")
        .header("x-ms-readonly", "true")
        .send(body)
        .map_err(|_| DeliveryProblem::Transport.error())?;
    if response.status().as_u16() != 200 {
        return Err(DeliveryProblem::Transport.error());
    }
    let bytes = response
        .body_mut()
        .with_config()
        .limit(response::MAX_RESPONSE_BYTES as u64)
        .read_to_vec()
        .map_err(|_| DeliveryProblem::Transport.error())?;
    response::parse(&bytes, columns, row_limit)
}
fn agent(origin: &str) -> &'static ureq::Agent {
    static CLOUD: OnceLock<ureq::Agent> = OnceLock::new();
    #[cfg(test)]
    static LOOPBACK: OnceLock<ureq::Agent> = OnceLock::new();
    #[cfg(test)]
    let secure = !origin.starts_with("http://127.0.0.1:");
    #[cfg(not(test))]
    let secure = {
        let _ = origin;
        true
    };
    #[cfg(test)]
    let cell = if secure { &CLOUD } else { &LOOPBACK };
    #[cfg(not(test))]
    let cell = &CLOUD;
    cell.get_or_init(|| {
        ureq::Agent::config_builder()
            .https_only(secure)
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(HTTP_TIMEOUT))
            .build()
            .into()
    })
}
