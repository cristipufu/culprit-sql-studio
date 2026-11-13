use super::super::{ActivityFrame, ActivityRecord};
use super::fields::signed_integer;
use crate::{
    read::{integer, invalid_data, optional_counter, optional_text, text},
    value::Rec,
};
use culprit_core::{
    ObservedWaitResource, Result,
    telemetry::{NativeIdentity, PageWaitRecheck, PageWaitRelation, PageWaitRequestState, canonical_time},
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

    pub(super) fn recheck(&self, record: &ActivityRecord, resource: &ObservedWaitResource) -> PageWaitRecheck {
        let request = self.request_state(record);
        let wait = if request == PageWaitRequestState::SameStatement {
            match self.wait_type.as_deref() {
                None => PageWaitRelation::NoLongerObserved,
                Some(native)
                    if Some(native) != record.facts.wait.as_ref().and_then(|wait| wait.native_type.as_deref()) =>
                {
                    PageWaitRelation::Different
                }
                Some(_) => match self.resource.as_deref().filter(|resource| !resource.is_empty()) {
                    Some(current) if current == resource.native.id => PageWaitRelation::OriginalObserved,
                    Some(_) => PageWaitRelation::Different,
                    None => PageWaitRelation::Unqualified,
                },
            }
        } else {
            PageWaitRelation::Unqualified
        };
        PageWaitRecheck { observed_at: self.at.clone(), request, wait }
    }

    fn request_state(&self, record: &ActivityRecord) -> PageWaitRequestState {
        if self.session.is_none() || self.request.is_none() {
            return PageWaitRequestState::Missing;
        }
        let key = &record.facts.key;
        let Some(request) = &key.request else { return PageWaitRequestState::Missing };
        let Some(locator) = &record.current_locator else { return PageWaitRequestState::Unverified };
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
            || expected_hash
                .is_some_and(|hash| !self.query_hash.as_ref().is_some_and(|current| current.eq_ignore_ascii_case(hash)))
        {
            PageWaitRequestState::Changed
        } else {
            PageWaitRequestState::SameStatement
        }
    }

    pub(super) fn same_plan(&self, other: &Self) -> bool {
        self.plan_handle == other.plan_handle
            && self.plan_hash == other.plan_hash
            && self.query_hash == other.query_hash
    }

    pub(super) fn identities(&self, other: &Self) -> Option<(NativeIdentity, Option<NativeIdentity>)> {
        let statement = NativeIdentity::new(self.sql_handle.as_ref()?)
            .qualified("native_object", "sys.dm_exec_requests.sql_handle")
            .qualified("statement_start_offset", self.begin?.to_string())
            .qualified("statement_end_offset", self.end?.to_string());
        let plan = self.plan_handle.as_ref().filter(|_| self.same_plan(other)).map(|handle| {
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
        let session = record.facts.key.session.id.parse::<i32>().map_err(|_| invalid_data())?;
        let request = record
            .facts
            .key
            .request
            .as_ref()
            .ok_or_else(invalid_data)?
            .id
            .parse::<i32>()
            .map_err(|_| invalid_data())?;
        xml.push_str(&format!("<row ordinal=\"{index}\" session=\"{session}\" request=\"{request}\"/>"));
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
