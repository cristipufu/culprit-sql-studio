//! Optional native plan facilities are selected from metadata, never from an arbitrary SQL error.
use crate::read::{NativeRead, ReadError, integer, invalid_data, optional_boolean};
use culprit_core::Result;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Availability {
    Available,
    Absent,
    Hidden,
}
pub(crate) struct Layout {
    pub last_actual: Availability,
    pub live_xml: Availability,
    pub profiles: Availability,
}
impl Layout {
    pub async fn read<R: NativeRead>(reader: &mut R) -> Result<Self> {
        let rows=reader.query("SELECT HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DEFINITION') AS metadata_visibility, CASE WHEN OBJECT_ID(N'sys.dm_exec_query_plan_stats') IS NULL THEN 0 ELSE 1 END AS last_actual, CASE WHEN OBJECT_ID(N'sys.dm_exec_query_statistics_xml') IS NULL THEN 0 ELSE 1 END AS live_xml, CASE WHEN OBJECT_ID(N'sys.dm_exec_query_profiles') IS NULL THEN 0 ELSE 1 END AS profiles OPTION(MAXDOP 1)",vec![],2).await.map_err(ReadError::into_error)?;
        let [row] = rows.as_slice() else { return Err(invalid_data()) };
        let visible = optional_boolean(row, "metadata_visibility")? == Some(true);
        let availability = |name| match integer(row, name)? {
            1 => Ok(Availability::Available),
            0 => Ok(if visible { Availability::Absent } else { Availability::Hidden }),
            _ => Err(invalid_data()),
        };
        Ok(Self {
            last_actual: availability("last_actual")?,
            live_xml: availability("live_xml")?,
            profiles: availability("profiles")?,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        resources::test_support::FakeRead,
        value::{Rec, Val},
    };
    #[tokio::test]
    async fn native_absence_hidden_metadata_and_query_failure_remain_distinct() {
        for (visible, expected) in [
            (Val::Int(1), Availability::Absent),
            (Val::Int(0), Availability::Hidden),
            (Val::Null, Availability::Hidden),
        ] {
            let mut reader = FakeRead::new(vec![Ok(vec![Rec::of(&[
                ("metadata_visibility", visible),
                ("last_actual", Val::Int(0)),
                ("live_xml", Val::Int(1)),
                ("profiles", Val::Int(0)),
            ])])]);
            let layout = Layout::read(&mut reader).await.unwrap();
            assert_eq!(layout.last_actual, expected);
            assert_eq!(layout.live_xml, Availability::Available);
            assert_eq!(layout.profiles, expected);
            assert_eq!(reader.calls.len(), 1);
        }
        let mut failed = FakeRead::new(vec![Err(ReadError::Source(invalid_data()))]);
        assert!(Layout::read(&mut failed).await.is_err());
        assert_eq!(failed.calls.len(), 1);
    }
}
