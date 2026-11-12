CREATE VIEW IF NOT EXISTS $FUNCTION AS
WITH Observations AS (
    SELECT *, $SOURCE AS NativeSource FROM CulpritPublishedV1($ARGS)
    $EXPAND
    WHERE RangeKind='current' AND $SELECTION $COMPLETE
), Epochs AS (SELECT *,max(OwnerEpoch) OVER (PARTITION BY $KEYS) AS HeadEpoch FROM Observations),
Owned AS (SELECT * FROM Epochs WHERE OwnerEpoch=HeadEpoch),
Sequences AS (SELECT *,max(CaptureSequence) OVER (PARTITION BY $KEYS) AS HeadSequence FROM Owned)
SELECT * EXCEPT (HeadEpoch,HeadSequence) FROM Sequences WHERE CaptureSequence=HeadSequence
