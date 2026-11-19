//! Snapshot consumers share native source admission and one exact, unambiguous workload-query binding.
use super::{QueryCompilation, cases};
use culprit_core::{
    QueryStat, Snapshot, SourceOutcome,
    telemetry::{CompilationSnapshot, MssqlQueryCompilation},
};

pub(crate) struct CapturedCompilations<'s> {
    pub read: &'s CompilationSnapshot,
    pub source: Option<&'s SourceOutcome>,
    pub cases: Vec<QueryCompilation<'s>>,
}

impl<'s> CapturedCompilations<'s> {
    pub fn from_snapshot(snapshot: &'s Snapshot) -> Option<Self> {
        let read = snapshot
            .operations
            .query_compilation
            .as_ref()
            .filter(|read| read.is_valid() && read.fits_database(&snapshot.database_identity()))?;
        let rows = read.observations.iter().collect::<Vec<_>>();
        let source = snapshot.source_outcome(&read.source);
        let cases = cases(&rows, source).ok()?;
        Some(Self { read, source, cases })
    }
}

pub(crate) fn workload_query<'s>(
    snapshot: &'s Snapshot,
    read: &CompilationSnapshot,
    native: &MssqlQueryCompilation,
) -> Option<&'s QueryStat> {
    if !read.matches_database_namespace(&snapshot.database_identity()) {
        return None;
    }
    let mut matches = snapshot
        .workload
        .iter()
        .flat_map(|workload| &workload.queries)
        .filter(|query| query.native_identity.as_ref() == Some(&native.identity));
    let query = matches.next()?;
    matches.next().is_none().then_some(query)
}
