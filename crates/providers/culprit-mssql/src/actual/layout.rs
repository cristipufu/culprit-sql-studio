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
    pub request_page_server_read_capability: crate::activity::page_server_reads::Availability,
}
impl Layout {
    pub async fn read<R: NativeRead>(reader: &mut R) -> Result<Self> {
        Self::read_detail(reader, Detail::PlanFacilities).await
    }
    pub(super) async fn read_request<R: NativeRead>(reader: &mut R) -> Result<Self> {
        Self::read_detail(reader, Detail::RequestCounters).await
    }
    async fn read_detail<R: NativeRead>(reader: &mut R, detail: Detail) -> Result<Self> {
        let rows = reader.query(&detail.query(), vec![], 2).await.map_err(ReadError::into_error)?;
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
            request_page_server_read_capability: match detail {
                Detail::PlanFacilities => crate::activity::page_server_reads::Availability::Unknown,
                Detail::RequestCounters => crate::activity::page_server_reads::capability(row)?,
            },
        })
    }
}
#[derive(Clone, Copy)]
enum Detail {
    PlanFacilities,
    RequestCounters,
}
impl Detail {
    fn query(self) -> String {
        let columns = "HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DEFINITION') AS metadata_visibility, CASE WHEN OBJECT_ID(N'sys.dm_exec_query_plan_stats') IS NULL THEN 0 ELSE 1 END AS last_actual, CASE WHEN OBJECT_ID(N'sys.dm_exec_query_statistics_xml') IS NULL THEN 0 ELSE 1 END AS live_xml, CASE WHEN OBJECT_ID(N'sys.dm_exec_query_profiles') IS NULL THEN 0 ELSE 1 END AS profiles";
        match self {
            Self::PlanFacilities => format!("SELECT {columns} OPTION(MAXDOP 1)"),
            Self::RequestCounters => format!(
                "SELECT /* culprit */ {columns},{} AS request_page_server_read_capability OPTION(MAXDOP 1)",
                crate::activity::page_server_reads::CAPABILITY
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        resources::test_support::FakeRead,
        value::{Rec, Val},
    };
    #[test]
    fn retained_plan_facility_template_stays_independent_of_request_counter_capabilities() {
        let sql = Detail::PlanFacilities.query();
        assert_eq!(
            culprit_core::telemetry::digest(sql.as_bytes()),
            "9e97811fadc09786fa23e21db7b932ff7bb8a218a26fc6719420ac9b014b66c2"
        );
        assert!(!sql.contains("sys.all_columns"));
        let requested = Detail::RequestCounters.query();
        assert!(requested.contains("/* culprit */"));
        assert!(requested.contains(crate::activity::page_server_reads::CAPABILITY));
    }

    #[tokio::test]
    async fn request_layout_explicitly_selects_optional_capability_in_its_existing_single_read() {
        use crate::activity::page_server_reads::Availability as PageAvailability;
        for (value, expected) in [
            (Val::Null, PageAvailability::Unknown),
            (Val::Int(0), PageAvailability::NotApplicable),
            (Val::Int(1), PageAvailability::Available),
            (Val::Int(2), PageAvailability::Unknown),
            (Val::Int(3), PageAvailability::Unavailable),
        ] {
            let mut reader = FakeRead::new(vec![Ok(vec![Rec::of(&[
                ("metadata_visibility", Val::Int(1)),
                ("last_actual", Val::Int(1)),
                ("live_xml", Val::Int(1)),
                ("profiles", Val::Int(1)),
                ("request_page_server_read_capability", value),
            ])])]);
            let layout = Layout::read_request(&mut reader).await.unwrap();
            assert_eq!(layout.request_page_server_read_capability, expected);
            assert_eq!(reader.calls.len(), 1);
            assert!(reader.calls[0].parameters.is_empty());
            assert_eq!(reader.calls[0].limit, 2);
        }
    }

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
            assert_eq!(
                layout.request_page_server_read_capability,
                crate::activity::page_server_reads::Availability::Unknown
            );
            assert_eq!(reader.calls.len(), 1);
        }
        let mut failed = FakeRead::new(vec![Err(ReadError::Source(invalid_data()))]);
        assert!(Layout::read(&mut failed).await.is_err());
        assert_eq!(failed.calls.len(), 1);
    }
}
