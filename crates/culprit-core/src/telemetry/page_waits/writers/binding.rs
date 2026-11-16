use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PageWriteOperation {
    Insert,
    Update,
    Delete,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlPageWriteAccess {
    pub node_id: u32,
    /// Native object/index access only. Neither the page nor a partition reached by this operator is measured.
    pub operation: PageWriteOperation,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlPageWritePlanProof {
    pub target_key: String,
    pub native_plan_sha256: String,
    pub native_statement_sha256: String,
    pub input_sha256: String,
    pub accesses: Vec<MssqlPageWriteAccess>,
}

impl MssqlPageWritePlanProof {
    pub fn new(
        target: &MappedPageTarget,
        candidate: &PageWaitWriterCandidate,
        native_plan_sha256: String,
        native_statement_sha256: String,
        mut accesses: Vec<MssqlPageWriteAccess>,
    ) -> Result<Self> {
        accesses.sort_by_key(|access| access.node_id);
        let target_key = target.writer_target_key()?;
        let input_sha256 = input(&target_key, &native_plan_sha256, &native_statement_sha256, candidate, &accesses)?;
        let value = Self { target_key, native_plan_sha256, native_statement_sha256, input_sha256, accesses };
        if !value.matches(target, candidate) {
            return Err(invalid());
        }
        Ok(value)
    }

    pub fn is_valid(&self) -> bool {
        [&self.target_key, &self.native_plan_sha256, &self.native_statement_sha256, &self.input_sha256]
            .into_iter()
            .all(|value| valid_digest(value))
            && (1..=MAX_PAGE_WAIT_WRITE_ACCESSES).contains(&self.accesses.len())
            && self.accesses.windows(2).all(|pair| pair[0].node_id < pair[1].node_id)
    }

    pub fn matches(&self, target: &MappedPageTarget, candidate: &PageWaitWriterCandidate) -> bool {
        self.is_valid()
            && target.writer_target_key().is_ok_and(|key| key == self.target_key)
            && candidate.request_state == PageWaitRequestState::SameStatement
            && candidate.plan_identity.as_ref().is_some_and(NativeIdentity::is_valid)
            && candidate.request.as_ref().is_some_and(|request| {
                request.is_valid() && request.statement.is_available() && request.statement_identity.is_some()
            })
            && candidate.plan.is_available()
            && self.input(candidate).is_ok_and(|input| input == self.input_sha256)
    }

    fn input(&self, candidate: &PageWaitWriterCandidate) -> Result<String> {
        input(&self.target_key, &self.native_plan_sha256, &self.native_statement_sha256, candidate, &self.accesses)
    }
}

fn input(
    target: &str,
    plan: &str,
    statement: &str,
    candidate: &PageWaitWriterCandidate,
    accesses: &[MssqlPageWriteAccess],
) -> Result<String> {
    let request = candidate.request.as_ref().ok_or_else(invalid)?;
    hash(&(
        "mssql-page-write-plan-v1",
        target,
        plan,
        statement,
        &candidate.parent,
        &request.key,
        &request.statement_identity,
        &request.statement,
        &candidate.plan_identity,
        &candidate.plan,
        accesses,
    ))
}
