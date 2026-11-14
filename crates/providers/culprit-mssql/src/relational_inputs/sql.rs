//! One direct lookup per distinct admitted plan in one independently executed post-payload query. Native cache
//! iteration is not repeated, and no expression masquerades as a second observation through optimizer reuse.
pub(super) const VERIFY: &str = r#"DECLARE /* culprit */ @plans xml=CONVERT(xml,@P1);
SELECT /* culprit */ CONVERT(varchar(130),selected.plan_handle,1) AS plan_handle,
 CONVERT(varchar(130),CONVERT(varbinary(64),binding.value),1) AS sql_handle
FROM @plans.nodes('/plans/plan') selector(x)
CROSS APPLY(SELECT CONVERT(varbinary(64),selector.x.value('@handle[1]','varchar(130)'),1) AS plan_handle) selected
OUTER APPLY(SELECT value FROM sys.dm_exec_plan_attributes(selected.plan_handle) WHERE attribute='sql_handle') binding
OPTION(MAXDOP 1,LOOP JOIN)"#;
