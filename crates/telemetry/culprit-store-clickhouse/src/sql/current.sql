CREATE VIEW IF NOT EXISTS $TABLECurrent AS
WITH Heads AS (
    SELECT $KEYS FROM $HEAD(contract={contract:String},store={store:String},resource={resource:String},
        captured_from={asof:DateTime64(9, 'UTC')}-toIntervalMillisecond({max_age_ms:UInt64}),asof={asof:DateTime64(9, 'UTC')})
    WHERE {max_age_ms:UInt64}>0 AND CollectedUntil>={asof:DateTime64(9, 'UTC')}-toIntervalMillisecond({max_age_ms:UInt64})
)
SELECT $COLUMNS FROM $TABLERows(contract={contract:String},store={store:String},resource={resource:String},
    captured_from={asof:DateTime64(9, 'UTC')}-toIntervalMillisecond({max_age_ms:UInt64}),asof={asof:DateTime64(9, 'UTC')})
WHERE ($KEYS) IN (SELECT $KEYS FROM Heads)
