//! ReplicaID names only the connected Hyperscale member. A role, logical database GUID or routed endpoint is
//! never a substitute. NULL retains the native unsupported/unavailable distinction as unknown identity.
use crate::{
    read::{invalid_data, optional_text},
    value::Rec,
};
use culprit_core::Result;

pub(crate) const ID_EXPRESSION: &str = "CONVERT(varchar(36),TRY_CONVERT(uniqueidentifier,CONVERT(nvarchar(128),DATABASEPROPERTYEX(DB_NAME(),'ReplicaID'))))";

pub(crate) fn id(row: &Rec, column: &str) -> Result<Option<String>> {
    let value = optional_text(row, column)?;
    if value.as_deref().is_some_and(|value| !native_guid(value)) {
        return Err(invalid_data());
    }
    Ok(value)
}

fn native_guid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) { byte == b'-' } else { byte.is_ascii_hexdigit() }
        })
        && value.bytes().any(|byte| byte != b'0' && byte != b'-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Val;

    #[test]
    fn native_replica_ids_preserve_unknown_and_reject_role_endpoint_and_zero_guid_substitutes() {
        for value in [Val::Null, Val::Text("91487244-730d-4b83-8e49-d482241c7288".into())] {
            let row = Rec::of(&[("replica_id", value.clone())]);
            let expected = match value {
                Val::Null => None,
                Val::Text(value) => Some(value),
                _ => unreachable!(),
            };
            assert_eq!(id(&row, "replica_id").unwrap(), expected);
        }
        for value in [
            "READ_ONLY",
            "secondary",
            "myserver.database.windows.net",
            "00000000-0000-0000-0000-000000000000",
            "91487244-730d-4b83-8e49-d482241c7288truncated",
        ] {
            assert!(id(&Rec::of(&[("replica_id", Val::Text(value.into()))]), "replica_id").is_err());
        }
    }
}
