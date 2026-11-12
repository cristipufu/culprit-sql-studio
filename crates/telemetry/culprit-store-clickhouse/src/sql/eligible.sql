CREATE VIEW IF NOT EXISTS $TABLEEligible AS
WITH Published AS (
    SELECT * FROM CulpritPublishedV1(contract={contract:String},store={store:String},resource={resource:String},captured_from={captured_from:DateTime64(9, 'UTC')},asof={asof:DateTime64(9, 'UTC')})
), Membership AS (
    SELECT RangeId, JSONExtractString(Batch,'batch_id') AS BatchId, JSONExtractInt(Batch,'rows') AS ExpectedRows,
        1 AS BatchKnown, ContractId AS PublishedContract, ResourceId AS PublishedResource, ResourceKind AS PublishedKind,
        ManifestSha256 AS PublishedManifest, SchemaDigest AS PublishedSchema,
        OutboxId AS PublishedOutbox, OwnerEpoch AS PublishedEpoch, CaptureSequence AS PublishedSequence
    FROM Published ARRAY JOIN JSONExtractArrayRaw(Batches) AS Batch
    WHERE JSONExtractString(Batch,'table')='$TABLE' AND JSONExtractBool(Batch,'delivery_required')
), Rows AS (
    SELECT DISTINCT $COLUMNS FROM $TABLE WHERE RangeId IN (SELECT RangeId FROM Published)
), Candidates AS (
    SELECT R.*,M.ExpectedRows FROM Rows AS R ALL INNER JOIN Membership AS M ON R.RangeId=M.RangeId AND R.BatchId=M.BatchId
), Invalid AS (
    SELECT R.RangeId FROM Rows AS R ALL LEFT JOIN Membership AS M ON R.RangeId=M.RangeId AND R.BatchId=M.BatchId
    WHERE ifNull(M.BatchKnown,0)!=1 OR R.RowOrdinal<=0 OR R.RowOrdinal>M.ExpectedRows
        OR R.ContractId!=M.PublishedContract OR R.ResourceId!=M.PublishedResource OR R.ResourceKind!=M.PublishedKind
        OR R.ManifestSha256!=M.PublishedManifest OR R.SchemaDigest!=M.PublishedSchema
        OR R.OutboxId!=M.PublishedOutbox OR R.OwnerEpoch!=M.PublishedEpoch OR R.CaptureSequence!=M.PublishedSequence
), Conflicts AS (
    SELECT RangeId FROM Candidates GROUP BY RangeId,BatchId,RowOrdinal HAVING count()!=1
), Complete AS (
    SELECT RangeId,BatchId FROM Candidates
    WHERE RangeId NOT IN (SELECT RangeId FROM Invalid) AND RangeId NOT IN (SELECT RangeId FROM Conflicts)
    GROUP BY RangeId,BatchId HAVING count()=any(ExpectedRows)
)
SELECT $COLUMNS FROM Rows WHERE (RangeId,BatchId) IN (SELECT RangeId,BatchId FROM Complete)
