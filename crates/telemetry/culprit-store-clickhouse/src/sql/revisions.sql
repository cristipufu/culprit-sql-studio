CREATE VIEW IF NOT EXISTS $TABLERows AS
WITH Rows AS (SELECT * FROM $TABLEEligible($ARGS)),
Epochs AS (SELECT *,max(OwnerEpoch) OVER (PARTITION BY SourceId,RecordId) AS HeadEpoch FROM Rows),
Owned AS (SELECT * FROM Epochs WHERE OwnerEpoch=HeadEpoch),
Sequences AS (SELECT *,max(CaptureSequence) OVER (PARTITION BY SourceId,RecordId) AS HeadSequence FROM Owned),
Captured AS (SELECT * FROM Sequences WHERE CaptureSequence=HeadSequence),
Versions AS (SELECT *,max(RepresentationVersion) OVER (PARTITION BY SourceId,RecordId) AS HeadVersion FROM Captured)
SELECT $COLUMNS FROM Versions WHERE RepresentationVersion=HeadVersion
