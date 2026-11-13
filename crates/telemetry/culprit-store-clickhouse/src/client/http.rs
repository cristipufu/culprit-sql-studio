use super::*;
use crate::{response, schema::quote};
use std::{io::Read, sync::OnceLock, time::Duration};
const MAX_EXECUTION_SECONDS: u32 = 30;
const MAX_THREADS: u32 = 2;
const MAX_READ_ROWS: u32 = 1_000_000;
const MAX_READ_BYTES: u32 = 256 * 1024 * 1024;
const MAX_MEMORY_BYTES: u32 = 256 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(45);
const MAX_ACKNOWLEDGEMENT_BYTES: u64 = 64 * 1024;

fn request(
    origin: &str,
    config: &ClickHouseConfig,
    credentials: &Credentials,
    id: &str,
) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
    agent(origin)
        .post(format!("{origin}/"))
        .header("X-ClickHouse-User", &credentials.username)
        .header("X-ClickHouse-Key", credentials.password.as_str())
        .header("Content-Type", "application/octet-stream")
        .header("Accept-Encoding", "identity")
        .query("database", &config.database)
        .query("query_id", id)
        .query("max_execution_time", MAX_EXECUTION_SECONDS.to_string())
        .query("timeout_overflow_mode", "throw")
        .query("max_memory_usage", MAX_MEMORY_BYTES.to_string())
        .query("max_threads", MAX_THREADS.to_string())
        .query("wait_end_of_query", "1")
        .query("send_progress_in_http_headers", "0")
}
pub(super) fn query(
    origin: &str,
    config: &ClickHouseConfig,
    credentials: &Credentials,
    id: &str,
    input: QueryRequest,
) -> Result<QueryTable> {
    let mut request = request(origin, config, credentials, id)
        .query("readonly", "1")
        .query("max_result_rows", MAX_QUERY_ROWS.to_string())
        .query("max_result_bytes", input.byte_limit.to_string())
        .query("result_overflow_mode", "throw")
        .query("read_overflow_mode", "throw")
        .query("max_rows_to_read", MAX_READ_ROWS.to_string())
        .query("max_bytes_to_read", MAX_READ_BYTES.to_string())
        .query("cancel_http_readonly_queries_on_client_close", "1")
        .query("output_format_json_quote_64bit_integers", "0");
    if config.insert_quorum.is_some() {
        request = request.query("select_sequential_consistency", "1");
    }
    for (name, value) in &input.parameters {
        request = request.query(format!("param_{name}"), escaped(value));
    }
    let mut response =
        request.send(format!("/* culprit */ {} FORMAT JSONCompact", input.text)).map_err(|_| transport())?;
    if response.status().as_u16() != 200 || response.headers().get("Content-Encoding").is_some_and(|v| v != "identity")
    {
        return Err(transport());
    }
    let bytes =
        response.body_mut().with_config().limit(input.byte_limit as u64).read_to_vec().map_err(|_| transport())?;
    response::parse(&bytes, &input.columns, input.row_limit, input.byte_limit)
}
pub(super) fn insert(
    origin: &str,
    config: &ClickHouseConfig,
    credentials: &Credentials,
    id: &str,
    table: &str,
    data: InsertData,
) -> Result<Submission> {
    let bytes = data.bytes();
    let mut request = request(origin, config, credentials, id)
        .header("Content-Length", bytes.to_string())
        .query("query", format!("/* culprit */ INSERT INTO {} FORMAT JSONEachRow", quote(table)))
        .query("async_insert", "0")
        .query("date_time_input_format", "best_effort")
        .query("input_format_skip_unknown_fields", "0")
        .query("input_format_allow_errors_num", "0")
        .query("input_format_allow_errors_ratio", "0")
        .query("input_format_null_as_default", "0");
    if let Some(quorum) = config.insert_quorum {
        request = request
            .query("insert_quorum", quorum.to_string())
            .query("insert_quorum_timeout", "30000")
            .query("insert_quorum_parallel", "0");
    }
    let result = match &data {
        InsertData::Control(bytes) => request.send(bytes),
        InsertData::Batch(batch) => {
            let file = std::fs::File::open(batch.path()).map_err(|_| transport())?;
            if !file.metadata().map_err(|_| transport())?.is_file()
                || file.metadata().map_err(|_| transport())?.len() != bytes
            {
                return Err(DeliveryProblem::Integrity.error());
            }
            request.send(ureq::SendBody::from_owned_reader(file.take(bytes)))
        }
    };
    let mut response = match result {
        Ok(response) => response,
        Err(_) => return Ok(Submission::Unknown),
    };
    if response.status().as_u16() != 200 {
        return Ok(Submission::Unknown);
    }
    let reply = match response.body_mut().with_config().limit(MAX_ACKNOWLEDGEMENT_BYTES).read_to_vec() {
        Ok(reply) => reply,
        Err(_) => return Ok(Submission::Unknown),
    };
    // Errors can arrive after HTTP 200. Any nonempty acknowledgement remains ambiguous and requires readback.
    Ok(if reply.iter().all(u8::is_ascii_whitespace) { Submission::Accepted } else { Submission::Unknown })
}
fn escaped(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\0' => escaped.push_str("\\0"),
            character => escaped.push(character),
        }
    }
    escaped
}
fn agent(origin: &str) -> &'static ureq::Agent {
    static SECURE: OnceLock<ureq::Agent> = OnceLock::new();
    static LOOPBACK: OnceLock<ureq::Agent> = OnceLock::new();
    let secure = origin.starts_with("https://");
    let cell = if secure { &SECURE } else { &LOOPBACK };
    cell.get_or_init(|| {
        ureq::Agent::config_builder()
            .https_only(secure)
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(TIMEOUT))
            .build()
            .into()
    })
}
