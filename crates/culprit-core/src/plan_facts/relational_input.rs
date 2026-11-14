//! Native relational-input classification. Names, scalar parameter types and estimates do not establish kind.

use serde::{Deserialize, Deserializer, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum RelationalInputKind {
    TableValuedParameter,
    LocalTableVariable,
    TemporaryTable,
    TableValuedFunction,
    Unknown,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum RelationalInputProvenance {
    /// An explicit native operator or qualified native temporary-object reference.
    NativePlan,
    /// An unambiguous declaration within the selected statement's native batch/scope.
    NativeDeclaration,
    /// Parameter/module/type metadata qualified to the native statement and module incarnation.
    NativeParameterMetadata,
    Unresolved,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct RelationalInput {
    pub kind: RelationalInputKind,
    pub provenance: RelationalInputProvenance,
}

impl RelationalInput {
    pub fn is_valid(self) -> bool {
        use RelationalInputKind::*;
        use RelationalInputProvenance::*;
        matches!(
            (self.kind, self.provenance),
            (TableValuedParameter, NativeParameterMetadata)
                | (LocalTableVariable, NativeDeclaration)
                | (TemporaryTable | TableValuedFunction, NativePlan)
                | (Unknown, Unresolved)
        )
    }

    /// Invalid or unqualified metadata cannot authorize kind-specific advice even for an in-memory embedder.
    pub fn qualified_kind(self) -> RelationalInputKind {
        if self.is_valid() { self.kind } else { RelationalInputKind::Unknown }
    }
}

pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<RelationalInput>, D::Error> {
    Ok(Option::<RelationalInput>::deserialize(deserializer)?.filter(|input| input.is_valid()))
}

#[cfg(test)]
mod tests;
