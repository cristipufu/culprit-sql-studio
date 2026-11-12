//! Only service-issued Azure SAS URLs receive uploads or queue messages. Kusto bearer tokens never accompany them.
use super::resources::Resources;
use crate::{http, ingestion::IngestionBatch};
use base64::{Engine, engine::general_purpose::STANDARD};
use culprit_core::{
    Result,
    telemetry::{DeliveryProblem, digest},
};
use serde_json::json;
use std::io::Read;
use zeroize::Zeroizing;

const STORAGE_VERSION: &str = "2023-11-03";
const MAX_QUEUE_MESSAGE_BYTES: usize = 64 * 1024;
const MAX_ACK_BYTES: u64 = 64 * 1024;

pub(super) fn deliver(resources: &Resources, index: usize, database: &str, batch: &IngestionBatch) -> Result<()> {
    if !resources.usable() {
        return Err(DeliveryProblem::Transport.error());
    }
    let container = &resources.containers[index % resources.containers.len()];
    let queue = &resources.queues[index % resources.queues.len()];
    let blob = container.url(batch.key());
    let message = message(database, batch, &blob, &resources.identity)?;
    upload(&container.origin, &blob, batch)?;
    enqueue(&queue.origin, &queue.url("messages"), message)
}
fn upload(origin: &str, location: &str, batch: &IngestionBatch) -> Result<()> {
    let reader = batch.reader()?;
    let mut response = http::agent(origin)
        .put(location)
        .header("x-ms-version", STORAGE_VERSION)
        .header("x-ms-date", storage_date())
        .header("x-ms-blob-type", "BlockBlob")
        .header("Content-Type", "application/x-ndjson")
        .header("Content-Length", batch.bytes().to_string())
        .header("x-ms-client-request-id", http::request_id()?)
        .send(ureq::SendBody::from_owned_reader(reader.take(batch.bytes())))
        .map_err(|_| DeliveryProblem::Transport.error())?;
    acknowledgement(&mut response)
}
fn enqueue(origin: &str, location: &str, message: Zeroizing<String>) -> Result<()> {
    let mut response = http::agent(origin)
        .post(location)
        .header("x-ms-version", STORAGE_VERSION)
        .header("x-ms-date", storage_date())
        .header("Content-Type", "application/xml; charset=utf-8")
        .header("x-ms-client-request-id", http::request_id()?)
        .send(message.as_bytes())
        .map_err(|_| DeliveryProblem::Transport.error())?;
    acknowledgement(&mut response)
}
fn acknowledgement(response: &mut ureq::http::Response<ureq::Body>) -> Result<()> {
    if response.status().as_u16() != 201 {
        return Err(DeliveryProblem::Transport.error());
    }
    response
        .body_mut()
        .with_config()
        .limit(MAX_ACK_BYTES)
        .read_to_vec()
        .map(|_| ())
        .map_err(|_| DeliveryProblem::Transport.error())
}
fn storage_date() -> String {
    chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}
fn message(database: &str, batch: &IngestionBatch, blob: &str, identity: &str) -> Result<Zeroizing<String>> {
    let id = digest(batch.key().as_bytes());
    let id = format!("{}-{}-{}-{}-{}", &id[..8], &id[8..12], &id[12..16], &id[16..20], &id[20..32]);
    let message = Zeroizing::new(
        json!({
            "Id":id, "BlobPath":blob, "RawDataSize":batch.bytes(),
            "DatabaseName":database, "TableName":batch.table(),
            "RetainBlobOnSuccess":false, "FlushImmediately":false, "ReportLevel":1, "ReportMethod":0,
            "AdditionalProperties":{"authorizationContext":identity,"format":"json",
                "mappingReference":format!("{}Mapping",batch.table())}
        })
        .to_string(),
    );
    let encoded = Zeroizing::new(STANDARD.encode(message.as_bytes()));
    if encoded.len() > MAX_QUEUE_MESSAGE_BYTES {
        return Err(DeliveryProblem::Capacity.error());
    }
    Ok(Zeroizing::new(format!("<QueueMessage><MessageText>{}</MessageText></QueueMessage>", encoded.as_str())))
}
