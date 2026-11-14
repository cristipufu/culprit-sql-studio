use super::{ActivityFrame, ActivityReader, ActivityRecord, RequestLocator};
use crate::read::{NativeRead, Parameter, ReadError};
use crate::{
    read::{integer, invalid_data, optional_counter, optional_text, text},
    value::Rec,
};
use culprit_core::{
    Result,
    telemetry::{NativeIdentity, canonical_time},
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RequestState {
    SameStatement,
    Missing,
    Changed,
    Unverified,
}

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
    pub(super) wait_type: Option<String>,
    pub(super) resource: Option<String>,
    plan_handle: Option<String>,
    query_hash: Option<String>,
    plan_hash: Option<String>,
}
impl Check {
    pub(super) fn from_record(record: &ActivityRecord, at: &str) -> Self {
        let request = record.facts.key.request.as_ref();
        let locator = record.current_locator.as_ref();
        let wait = record.facts.wait.as_ref();
        Self {
            ordinal: 0,
            at: at.into(),
            session: Some(record.facts.key.session.id.clone()),
            login: Some(record.facts.key.session.generation.clone()),
            request: request.map(|request| request.id.clone()),
            started: request.map(|request| request.generation.clone()),
            sql_handle: locator.map(|locator| locator.handle.clone()),
            begin: locator.map(|locator| locator.start),
            end: locator.map(|locator| locator.end),
            database: locator.map(|locator| locator.database),
            wait_type: wait.and_then(|wait| wait.native_type.clone()),
            resource: wait
                .and_then(|wait| wait.identified_resources.first().map(|resource| resource.native.id.clone())),
            plan_handle: None,
            query_hash: record.facts.query_hash.clone(),
            plan_hash: None,
        }
    }

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

    pub(super) fn request_state(&self, record: &ActivityRecord) -> RequestState {
        self.matches_request(&record.facts, record.current_locator.as_ref())
    }

    fn matches_request(
        &self,
        original: &culprit_core::telemetry::ActivityObservation,
        locator: Option<&RequestLocator>,
    ) -> RequestState {
        if self.session.is_none() || self.request.is_none() {
            return RequestState::Missing;
        }
        let key = &original.key;
        let Some(request) = &key.request else { return RequestState::Missing };
        let Some(locator) = locator else { return RequestState::Unverified };
        let expected_hash = original.query_hash.as_deref().filter(|value| !value.bytes().all(|value| value == b'0'));
        let Some(current) = self.statement_locator() else { return RequestState::Unverified };
        if self.login.is_none() || self.started.is_none() || expected_hash.is_some() && self.query_hash.is_none() {
            return RequestState::Unverified;
        }
        if self.session.as_deref() != Some(key.session.id.as_str())
            || self.login.as_deref() != Some(key.session.generation.as_str())
            || self.request.as_deref() != Some(request.id.as_str())
            || self.started.as_deref() != Some(request.generation.as_str())
            || !current.matches(locator)
            || expected_hash
                .is_some_and(|hash| !self.query_hash.as_ref().is_some_and(|current| current.eq_ignore_ascii_case(hash)))
        {
            RequestState::Changed
        } else {
            RequestState::SameStatement
        }
    }

    pub(super) fn observation_state(&self, original: &culprit_core::telemetry::ActivityObservation) -> RequestState {
        let Some(identity) = original.statement_identity.as_ref() else { return RequestState::Unverified };
        let Some(locator) = RequestLocator::from_identity(identity) else { return RequestState::Unverified };
        self.matches_request(original, Some(&locator))
    }

    fn same_plan(&self, other: &Self) -> bool {
        self.plan_handle == other.plan_handle
            && self.plan_hash == other.plan_hash
            && self.query_hash == other.query_hash
    }

    fn statement_locator(&self) -> Option<RequestLocator> {
        RequestLocator::from_native(self.sql_handle.clone()?, self.begin?, self.end?, self.database?)
    }

    pub(super) fn identities(&self, other: &Self) -> Option<(NativeIdentity, Option<NativeIdentity>)> {
        let statement = self.statement_locator()?.identity();
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

fn signed_integer(row: &Rec, name: &str) -> Result<Option<i32>> {
    crate::read::optional_signed_integer(row, name)?
        .map(|value| i32::try_from(value).map_err(|_| invalid_data()))
        .transpose()
}

const PARTICIPANTS: &str = r#"
DECLARE /* culprit */ @targets xml=CAST(@P1 AS xml),@at datetime2=SYSUTCDATETIME();
SELECT /* culprit */ a.ordinal,CONVERT(varchar(40),@at,126)+'Z' AS observed_at,
 r.session_id AS checked_session_id,CONVERT(varchar(40),CAST(s.login_time AS datetime2),126) AS checked_session_generation,
 r.request_id AS checked_request_id,CONVERT(varchar(40),CAST(r.start_time AS datetime2),126) AS checked_request_generation,
 CONVERT(varchar(128),r.sql_handle,2) AS checked_sql_handle,r.statement_start_offset AS checked_begin,r.statement_end_offset AS checked_end,
 r.database_id AS checked_database_id,r.wait_type AS checked_wait_type,r.wait_resource AS checked_wait_resource,
 CONVERT(varchar(128),r.plan_handle,2) AS checked_plan_handle,CONVERT(varchar(16),r.query_hash,2) AS checked_query_hash,
 CONVERT(varchar(16),r.query_plan_hash,2) AS checked_plan_hash
FROM @targets.nodes('/activity/row') t(n)
CROSS APPLY(SELECT t.n.value('@ordinal[1]','int') AS ordinal,t.n.value('@session[1]','int') AS session_id,
 t.n.value('@request[1]','int') AS request_id) a
LEFT JOIN sys.dm_exec_sessions s ON s.session_id=a.session_id
LEFT JOIN sys.dm_exec_requests r ON r.session_id=a.session_id AND r.request_id=a.request_id
OPTION(MAXDOP 1,LOOP JOIN)"#;

impl<R: NativeRead> ActivityReader<R> {
    pub(super) async fn request_participants(
        &mut self,
        frame: &ActivityFrame,
        indices: &[usize],
    ) -> Result<BTreeMap<usize, Check>> {
        if indices.is_empty() {
            return Ok(BTreeMap::new());
        }
        let selected: BTreeSet<_> = indices.iter().copied().collect();
        let rows = self
            .io
            .query(PARTICIPANTS, vec![Parameter::Text(selectors(frame, indices)?)], indices.len())
            .await
            .map_err(ReadError::into_error)?;
        if rows.len() != indices.len() {
            return Err(invalid_data());
        }
        let mut checked = BTreeMap::new();
        for row in &rows {
            let proof = Check::decode(row)?;
            if !selected.contains(&proof.ordinal) || checked.insert(proof.ordinal, proof).is_some() {
                return Err(invalid_data());
            }
        }
        Ok(checked)
    }
}
