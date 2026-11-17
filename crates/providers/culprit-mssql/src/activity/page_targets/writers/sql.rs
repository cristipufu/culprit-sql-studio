//! Finite original selectors. Native payload functions receive only checked request handles and byte offsets.
use super::*;

pub(super) fn requests(metadata: bool, availability: crate::activity::page_server_reads::Availability) -> String {
    let columns = if metadata {
        format!(
            ",{},{} AS request_page_server_read_capability,CONVERT(varchar(40),SYSDATETIME(),126) AS native_observed_at,0 AS no_current_request,CASE WHEN r.database_id=DB_ID() THEN 1 ELSE 5 END AS scope_basis,CAST(NULL AS bigint) AS transaction_id,CAST(NULL AS varchar(40)) AS transaction_start",
            crate::activity::sql::request_columns(availability),
            crate::activity::page_server_reads::CAPABILITY,
        )
    } else {
        String::new()
    };
    requests::participant_query(
        &columns,
        if metadata { "LEFT JOIN sys.dm_exec_sessions b ON b.session_id=r.blocking_session_id" } else { "" },
    )
}

pub(super) fn payload_selectors(focus: &PageWaitWriterFocus, before: &BTreeMap<usize, RequestPoint>) -> Result<String> {
    let mut xml = String::from("<writers>");
    let mut count = 0;
    for (index, selected) in focus.candidates.iter().enumerate() {
        let point = before.get(&index).ok_or_else(invalid_data)?;
        if point.record.is_none() {
            continue;
        }
        let Some((_, Some(plan))) = point.check.identities(&point.check) else { continue };
        let locator = RequestLocator::from_identity(&selected.statement_identity).ok_or_else(invalid_data)?;
        let request = selected.key.request.as_ref().ok_or_else(invalid_data)?;
        xml.push_str(&format!("<row ordinal=\"{index}\" session=\"{}\" login=\"{}\" request=\"{}\" started=\"{}\" handle=\"{}\" begin=\"{}\" end=\"{}\" database=\"{}\" plan=\"{}\"/>",selected.key.session.id,crate::sql_time::native_time(&selected.key.session.generation)?,request.id,crate::sql_time::native_time(&request.generation)?,locator.handle,locator.start,locator.end,locator.database,plan.id));
        count += 1;
    }
    xml.push_str("</writers>");
    Ok(if count == 0 { String::new() } else { xml })
}

pub(super) fn payloads() -> String {
    let statement = crate::cached_statements::statement_expression("r", "st.text");
    let statement_columns = crate::cached_payload::columns("fragment.statement", "statement_bytes", "statement", "@P2");
    let plan_columns = crate::cached_payload::columns("qp.query_plan", "plan_bytes", "plan", "@P3");
    format!(
        r#"
DECLARE /* culprit */ @selected xml=CAST(@P1 AS xml);
SELECT /* culprit */ a.ordinal,st.encrypted,{statement_columns},{plan_columns}
FROM @selected.nodes('/writers/row') selected(n)
CROSS APPLY(SELECT n.value('@ordinal[1]','int') AS ordinal,n.value('@session[1]','int') AS session_id,
 n.value('@login[1]','varchar(40)') AS login_time,n.value('@request[1]','int') AS request_id,
 n.value('@started[1]','varchar(40)') AS start_time,n.value('@handle[1]','varchar(128)') AS sql_handle,
 n.value('@begin[1]','int') AS statement_start_offset,n.value('@end[1]','int') AS statement_end_offset,
 n.value('@database[1]','int') AS database_id,n.value('@plan[1]','varchar(128)') AS plan_handle) a
LEFT JOIN sys.dm_exec_sessions s ON s.session_id=a.session_id
 AND s.login_time=CAST(CONVERT(datetime2,a.login_time,126) AS datetime)
LEFT JOIN sys.dm_exec_requests r ON r.session_id=s.session_id AND r.request_id=a.request_id
 AND r.start_time=CAST(CONVERT(datetime2,a.start_time,126) AS datetime)
 AND r.database_id=DB_ID() AND r.database_id=a.database_id
 AND r.sql_handle=CONVERT(varbinary(64),a.sql_handle,2)
 AND r.plan_handle=CONVERT(varbinary(64),a.plan_handle,2)
 AND r.statement_start_offset=a.statement_start_offset AND r.statement_end_offset=a.statement_end_offset
OUTER APPLY sys.dm_exec_sql_text(r.sql_handle) st
OUTER APPLY(SELECT {statement} AS statement) fragment
OUTER APPLY sys.dm_exec_text_query_plan(r.plan_handle,r.statement_start_offset,r.statement_end_offset) qp
OPTION(MAXDOP 1,LOOP JOIN)"#
    )
}
