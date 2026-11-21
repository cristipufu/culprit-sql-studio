//! Positive native permission checks do not infer authority from an endpoint or login label.
pub(super) fn server_state() -> &'static str {
    r#"CASE WHEN CAST(SERVERPROPERTY('EngineEdition') AS int)=5 THEN
 CASE WHEN DB_NAME()<>'master' AND IS_SRVROLEMEMBER('##MS_ServerStateReader##')=1
  AND HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DATABASE STATE')=1 THEN 1 ELSE 0 END
 ELSE CASE WHEN CAST(PARSENAME(CONVERT(varchar(128),SERVERPROPERTY('ProductVersion')),4) AS int)>=16
  THEN CASE WHEN HAS_PERMS_BY_NAME(NULL,NULL,'VIEW SERVER PERFORMANCE STATE')=1 THEN 1 ELSE 0 END
  ELSE CASE WHEN HAS_PERMS_BY_NAME(NULL,NULL,'VIEW SERVER STATE')=1 THEN 1 ELSE 0 END END END"#
}

/// Only these native instance views document the unfiltered request and connection population needed
/// to identify an idle session. Azure SQL Database's public request contract is connection-scoped;
/// neither an Azure role, service tier nor a successful permission call proves the stronger population.
pub(super) fn connection_scope() -> &'static str {
    "CASE WHEN CAST(SERVERPROPERTY('EngineEdition') AS int) IN(2,3,4,8) AND CAST(PARSENAME(CONVERT(varchar(128),SERVERPROPERTY('ProductVersion')),4) AS int)>=11 THEN CAST(1 AS bit) WHEN CAST(SERVERPROPERTY('EngineEdition') AS int)=5 THEN CAST(0 AS bit) ELSE CAST(NULL AS bit) END"
}

/// The unchanged task positive-admission expression is reused only inside a documented instance scope.
/// Native permission NULL is retained independently from a rejected or unknown population scope.
pub(super) fn connections() -> String {
    let admitted = server_state();
    format!(
        r#"CASE WHEN @idle_connection_scope=1 THEN
 CASE WHEN ({admitted})=1 THEN CAST(1 AS bit)
 WHEN CAST(PARSENAME(CONVERT(varchar(128),SERVERPROPERTY('ProductVersion')),4) AS int)>=16
  THEN CONVERT(bit,HAS_PERMS_BY_NAME(NULL,NULL,'VIEW SERVER PERFORMANCE STATE'))
 ELSE CONVERT(bit,HAS_PERMS_BY_NAME(NULL,NULL,'VIEW SERVER STATE')) END
 ELSE CAST(NULL AS bit) END"#
    )
}
