/** A native reference must locate one row from the declared source; ambiguity never chooses a representative. */
export function observationRows<Row extends { source: string }>(
  observations: readonly Row[], source: string, reference: (row: Row) => string,
): ReadonlyMap<string, Row | null> {
  const rows = new Map<string, Row | null>();
  for (const row of observations) {
    if (row.source !== source) continue;
    insertUnique(rows, reference(row), row);
  }
  return rows;
}

/** Equal row keys in different sources are independent; repeated keys in one source stay ambiguous. */
export function observationRowsBySource<Row extends { source: string }>(
  observations: readonly Row[], reference: (row: Row) => string,
): ReadonlyMap<string, ReadonlyMap<string, Row | null>> {
  const sources = new Map<string, Map<string, Row | null>>();
  for (const row of observations) {
    let rows = sources.get(row.source);
    if (!rows) {
      rows = new Map();
      sources.set(row.source, rows);
    }
    insertUnique(rows, reference(row), row);
  }
  return sources;
}

function insertUnique<Row>(rows: Map<string, Row | null>, key: string, row: Row): void {
  rows.set(key, rows.has(key) ? null : row);
}
