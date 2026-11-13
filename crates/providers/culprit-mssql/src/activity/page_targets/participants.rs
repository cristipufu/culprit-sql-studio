use super::super::{ActivityFrame, ActivityRecord};
use super::fields::signed_integer;
use crate::{
    read::{integer, invalid_data, optional_counter, optional_text, text},
    value::Rec,
};
use culprit_core::{
    ObservedWaitResource, Result,
    telemetry::{NativeIdentity, PageWaitResolution, canonical_time},
};

#[derive(Clone)]
pub(super) struct Check {
    pub ordinal: usize,
    pub at: String,
    session: Option<String>,
    login: Option<String>,
    request: Option<String>,
    started: Option<String>,
    sql_handle: Option<String>,
    begin: Option<i32>,
    end: Option<i32>,
    database: Option<i32>,
    wait_type: Option<String>,
    resource: Option<String>,
    plan_handle: Option<String>,
    query_hash: Option<String>,
    plan_hash: Option<String>,
}
impl Check {
    pub(super) fn decode(row: &Rec) -> Result<Self> {
        Ok(Self {
            ordinal: usize::try_from(integer(row, "ordinal")?).map_err(|_| invalid_data())?,
            at: canonical_time(&text(row, "observed_at")?)?,
            session: optional_counter(row, "checked_session_id")?,
            login: native_time(row, "checked_session_generation")?,
            request: optional_counter(row, "checked_request_id")?,
            started: native_time(row, "checked_request_generation")?,
            sql_handle: handle(row, "checked_sql_handle", 128)?,
            begin: signed_integer(row, "checked_begin")?,
            end: signed_integer(row, "checked_end")?,
            database: signed_integer(row, "checked_database_id")?,
            wait_type: optional_text(row, "checked_wait_type")?,
            resource: optional_text(row, "checked_wait_resource")?,
            plan_handle: handle(row, "checked_plan_handle", 128)?,
            query_hash: handle(row, "checked_query_hash", 16)?,
            plan_hash: handle(row, "checked_plan_hash", 16)?,
        })
    }

    pub(super) fn resolution(
        &self,
        record: &ActivityRecord,
        resource: &ObservedWaitResource,
    ) -> Option<PageWaitResolution> {
        if self.session.is_none() || self.request.is_none() {
            return Some(PageWaitResolution::MissingRequest);
        }
        let key = &record.facts.key;
        let Some(request) = &key.request else { return Some(PageWaitResolution::MissingRequest) };
        let Some(locator) = &record.current_locator else { return Some(PageWaitResolution::UnverifiedSource) };
        let native_wait = record.facts.wait.as_ref().and_then(|wait| wait.native_type.as_deref());
        let expected_hash =
            record.facts.query_hash.as_deref().filter(|value| !value.bytes().all(|value| value == b'0'));
        if self.session.as_deref() != Some(key.session.id.as_str())
            || self.login.as_deref() != Some(key.session.generation.as_str())
            || self.request.as_deref() != Some(request.id.as_str())
            || self.started.as_deref() != Some(request.generation.as_str())
            || !self.sql_handle.as_ref().is_some_and(|handle| handle.eq_ignore_ascii_case(&locator.handle))
            || self.begin != Some(locator.start)
            || self.end != Some(locator.end)
            || self.database != Some(locator.database)
            || self.wait_type.as_deref() != native_wait
            || self.resource.as_deref() != Some(resource.native.id.as_str())
            || expected_hash
                .is_some_and(|hash| !self.query_hash.as_ref().is_some_and(|current| current.eq_ignore_ascii_case(hash)))
        {
            Some(PageWaitResolution::ChangedRequest)
        } else {
            None
        }
    }

    pub(super) fn same_plan(&self, other: &Self) -> bool {
        self.plan_handle == other.plan_handle
            && self.plan_hash == other.plan_hash
            && self.query_hash == other.query_hash
    }

    pub(super) fn identities(&self) -> Option<(NativeIdentity, Option<NativeIdentity>)> {
        let statement = NativeIdentity::new(self.sql_handle.as_ref()?)
            .qualified("native_object", "sys.dm_exec_requests.sql_handle")
            .qualified("statement_start_offset", self.begin?.to_string())
            .qualified("statement_end_offset", self.end?.to_string());
        let plan = self.plan_handle.as_ref().map(|handle| {
            let mut identity =
                NativeIdentity::new(handle).qualified("native_object", "sys.dm_exec_requests.plan_handle");
            if let Some(hash) = &self.plan_hash {
                identity = identity.qualified("query_plan_hash", hash);
            }
            identity
        });
        Some((statement, plan))
    }
}

pub(super) fn selectors(frame: &ActivityFrame, indices: &[usize]) -> Result<String> {
    let mut xml = String::from("<activity>");
    for &index in indices {
        let record = frame.records.get(index).ok_or_else(invalid_data)?;
        xml.push_str(&format!("<row ordinal=\"{index}\" {}/>", super::super::current_selector_attributes(record)?));
    }
    xml.push_str("</activity>");
    Ok(xml)
}
fn native_time(row: &Rec, name: &str) -> Result<Option<String>> {
    optional_text(row, name)?.map(|value| crate::sql_time::native_time(&value)).transpose()
}
fn handle(row: &Rec, name: &str, max: usize) -> Result<Option<String>> {
    let Some(value) = optional_text(row, name)? else { return Ok(None) };
    if value.len() > max || !value.len().is_multiple_of(2) || !value.bytes().all(|value| value.is_ascii_hexdigit()) {
        return Err(invalid_data());
    }
    Ok((!value.is_empty() && !value.bytes().all(|value| value == b'0')).then_some(value))
}
