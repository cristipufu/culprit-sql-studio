use super::*;

pub(super) fn explain(snapshot: &mut Snapshot) -> Option<Box<culprit_core::StatementWorkReport>> {
    let captured = snapshot.operations.statement_work.as_ref()?;
    let source = captured.source.clone();
    let same_database = captured.context.as_deref().is_none_or(|context| {
        context.target.database().is_some_and(|database| {
            database.engine == snapshot.server.engine
                && database.database == snapshot.server.database
                && snapshot.identity.as_ref().is_none_or(|identity| identity.comparison(database).allows_comparison())
        })
    });
    let result = if same_database {
        crate::statement_work::explain(captured, true).map(Box::new)
    } else {
        Err(culprit_core::Error::invalid(t!("view.statement_work.invalid")))
    };
    admit_explanation(snapshot, source, culprit_core::message!("view.statement_work.invalid"), result)
}

#[cfg(test)]
mod tests;
