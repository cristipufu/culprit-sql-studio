CREATE VIEW IF NOT EXISTS CulpritPublishedV1 AS
WITH Observations AS (
    SELECT DISTINCT $COLUMNS FROM CulpritPublishedRangesV1
    WHERE ContractId={contract:String} AND RecoveryStoreId={store:String} AND ResourceId={resource:String}
        AND SchemaDigest='$SCHEMA'
        AND CollectedUntil>={captured_from:DateTime64(9, 'UTC')} AND CollectedUntil<={asof:DateTime64(9, 'UTC')}
), Unambiguous AS (
    SELECT RangeId FROM Observations GROUP BY RangeId HAVING count()=1
)
SELECT $COLUMNS FROM Observations WHERE RangeId IN (SELECT RangeId FROM Unambiguous)
