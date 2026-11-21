//! Fixed MI catalog/statistics only. TOP bounds retained output, never a claim about internal native work.
use super::layout::{Field, Layout};
use culprit_core::telemetry::MssqlCatalogAvailability;

pub(super) fn layout() -> String {
    let names = Field::ALL.into_iter().map(|field| format!("N'{}'", field.name())).collect::<Vec<_>>().join(",");
    format!("SELECT /* culprit */ HAS_PERMS_BY_NAME(NULL,NULL,N'VIEW SERVER STATE') AS server_state,
 HAS_PERMS_BY_NAME(NULL,NULL,N'VIEW SERVER PERFORMANCE STATE') AS server_performance_state,
 OBJECT_ID(N'master.sys.server_resource_stats') AS native_object_id,
 c.name AS native_column,c.system_type_id AS native_type,c.max_length AS native_length,c.precision AS native_precision,c.scale AS native_scale
 FROM (VALUES(1)) AS root(n) OUTER APPLY (SELECT name,system_type_id,max_length,precision,scale
 FROM master.sys.all_columns WHERE object_id=OBJECT_ID(N'master.sys.server_resource_stats') AND name IN ({names})) AS c OPTION(MAXDOP 1)")
}
pub(super) fn samples(layout: &Layout) -> String {
    let columns = Field::ALL
        .into_iter()
        .map(|field| {
            let name = field.name();
            let expression = if layout.status(field) != MssqlCatalogAvailability::Available {
                if matches!(field, Field::Start | Field::End) {
                    "CAST(NULL AS varchar(40))".into()
                } else {
                    "CAST(NULL AS nvarchar(256))".into()
                }
            } else if matches!(field, Field::Start | Field::End) {
                format!("CONVERT(varchar(40),CAST(s.{name} AS datetime2(7)),126)+'Z'")
            } else if matches!(field, Field::ResourceType | Field::ResourceName | Field::Sku | Field::Hardware) {
                format!("s.{name}")
            } else {
                format!("CONVERT(varchar(40),s.{name})")
            };
            format!("{expression} AS {name}")
        })
        .collect::<Vec<_>>()
        .join(
            ",
 ",
        );
    format!(
        "DECLARE @observed_at datetime2(7)=SYSUTCDATETIME();
 SELECT /* culprit */ {},s.present,{columns}
 FROM sys.dm_os_sys_info AS i OUTER APPLY (SELECT TOP (@P3) 1 AS present,{}
 FROM master.sys.server_resource_stats
 WHERE end_time>=CAST(CONVERT(datetimeoffset,@P1,127) AS datetime2)
 AND end_time<CAST(CONVERT(datetimeoffset,@P2,127) AS datetime2)
 ORDER BY end_time ASC) AS s OPTION(MAXDOP 1)",
        crate::instance::point_columns(crate::instance::PointClock::ManagedInstanceRead),
        Field::ALL
            .into_iter()
            .filter(|field| layout.status(*field) == MssqlCatalogAvailability::Available)
            .map(|field| field.name())
            .collect::<Vec<_>>()
            .join(",")
    )
}
