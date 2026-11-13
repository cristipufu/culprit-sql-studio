/** A native reference must locate one row from the declared source; ambiguity never chooses a representative. */
export function observationRows<Row extends { source: string }>(
  observations: readonly Row[], source: string, reference: (row: Row) => string,
): ReadonlyMap<string, Row | null> {
  const rows = new Map<string, Row | null>();
  for (const row of observations) {
    if (row.source !== source) continue;
    const key = reference(row);
    rows.set(key, rows.has(key) ? null : row);
  }
  return rows;
}
