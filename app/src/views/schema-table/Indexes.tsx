import { ArrowRight, ChevronRight } from "lucide-react";
import { Fragment, useId, useMemo, useState } from "react";
import type { IndexNote } from "../../gen/IndexNote";
import type { IndexStat } from "../../gen/IndexStat";
import type { Report } from "../../gen/Report";
import type { TableStat } from "../../gen/TableStat";
import { bytes, count, oneLine } from "../../lib/format";
import { indexKey, indexUsage, plannedQueryCount } from "../../lib/index-usage";
import { setState } from "../../lib/store";

/** Each index: keys, the captured queries that use it, reads and writes, size, and what its counters do and don't show. */
export function Indexes({ report, table, indexes }: { report: Report; table: TableStat; indexes: IndexStat[] }) {
  const engine = report.snapshot.server.engine;
  const usageId = useId();
  const usage = useMemo(() => indexUsage(report), [report]);
  const [open, setOpen] = useState<string | null>(null);
  const planned = plannedQueryCount(report);
  const hasUsage = indexes.some((index) => index.reads != null || index.writes != null);
  // What the counters cover and what each index's counters say, as the index checks decide them.
  const sinceText = report.summary.index_counting;
  const notesOf = useMemo(() => {
    const byIndex = new Map<string, IndexNote[]>();
    for (const note of report.summary.index_notes) {
      const key = indexKey(note.schema, note.table, note.index);
      byIndex.set(key, [...(byIndex.get(key) ?? []), note]);
    }
    return (i: IndexStat) => byIndex.get(indexKey(i.schema, i.table, i.name)) ?? [];
  }, [report]);
  const biggest = Math.max(1, ...indexes.map((i) => i.size_bytes ?? 0));
  if (indexes.length === 0)
    return (
      <div className="notice">
        <div>
          {table.kind === "virtual" ? (
            <><b>Indexes are managed by the virtual table's module.</b> Ordinary CREATE INDEX does not apply to this table.</>
          ) : (
            <><b>No index definitions in this scan.</b> Explain the queries that read this table to check their access paths.</>
          )}
        </div>
      </div>
    );
  return (
    <>
      {/* biome-ignore lint/a11y/noNoninteractiveTabindex: a scrolling region takes focus so the keyboard can scroll it */}
      <div className="td-grid-wrap" role="region" aria-label="Indexes" tabIndex={0}>
        <table className="grid td-grid ix-grid">
          <colgroup>
            <col className="index-name-col" />
            <col />
            <col className="index-used-col" />
            <col className="index-activity-col" />
            <col className="index-size-col" />
          </colgroup>
          <thead>
            <tr>
              <th>Index</th>
              <th>Keys</th>
              <th>Used by</th>
              <th title={hasUsage ? `Reads and writes ${sinceText}` : "Index usage counters are unavailable in this scan"}>Reads · writes</th>
              <th className="r">Size</th>
            </tr>
          </thead>
          <tbody>
            {indexes.map((i, position) => {
              const usesId = `${usageId}-${position}`;
              const uses = usage.get(indexKey(i.schema, i.table, i.name)) ?? [];
              const expanded = open === i.name && uses.length > 0;
              const reads = i.reads ?? 0;
              const writes = i.writes ?? 0;
              const rw = Math.max(1, reads + writes);
              const notes = notesOf(i);
              return (
                <Fragment key={i.name}>
                  <tr className={`row ${expanded ? "selected" : ""} ${notes.length > 0 ? "ix-has-notes" : ""}`}>
                    <td className="ix-name">
                      <div className="cn">
                        <span className="mono" title={i.name}>
                          {i.name}
                        </span>
                        {i.is_primary_key && <span className="tag">PK</span>}
                        {i.is_unique && !i.is_primary_key && <span className="tag">unique</span>}
                        {i.is_clustered && !i.is_primary_key && <span className="tag">clustered</span>}
                      </div>
                    </td>
                    <td className="ix-keys mono">
                      <span className="ix-key-list" title={i.columns.join(", ")}>{i.columns.join(", ")}</span>
                      {i.docdb && <div className="muted">{i.docdb.hash_columns.length ? `HASH (${i.docdb.hash_columns.join(", ")})` : "range sharded"}{i.docdb.tablets != null ? ` · ${i.docdb.tablets} tablet${i.docdb.tablets === 1 ? "" : "s"}` : ""}</div>}
                      {i.include.length > 0 && (
                        <div className="muted" title={`INCLUDE (${i.include.join(", ")})`}>
                          + {i.include.join(", ")}
                        </div>
                      )}
                      {i.filter && (
                        <div className="muted" title={i.filter}>
                          where {i.filter}
                        </div>
                      )}
                    </td>
                    <td className="ix-used">
                      {uses.length > 0 ? (
                        <button type="button" className="used-btn" onClick={() => setOpen(expanded ? null : i.name)} aria-expanded={expanded} aria-controls={expanded ? usesId : undefined} title={`${uses.length} captured ${uses.length === 1 ? "query uses" : "queries use"} ${i.name}`}>
                          <ChevronRight size={12} className={expanded ? "rot" : ""} />
                          <b>{count(uses.length)}</b> {uses.length === 1 ? "query" : "queries"}
                        </button>
                      ) : (
                        <span
                          className="muted"
                          title={
                            planned === 0
                              ? "Culprit has no query plans for this database yet, so it can't tell which queries use each index. Its read count is on the right."
                              : `None of the ${planned} plans Culprit captured (the heaviest queries) use it; lighter queries may still read it, as the read count shows.`
                          }
                        >
                          {planned === 0 ? "no plans yet" : (i.reads ?? 0) > 0 ? "lighter queries" : "no query"}
                        </span>
                      )}
                    </td>
                    <td className="ix-rw">
                      {i.reads == null && i.writes == null ? (
                        <span className="muted">—</span>
                      ) : (
                        <span className="rw" title={`${i.reads == null ? "unmeasured" : count(reads)} reads · ${i.writes == null ? "unmeasured" : count(writes)} writes ${sinceText}`}>
                          <span className="rw-bar">
                            <span className="r" style={{ width: `${(reads / rw) * 100}%` }} />
                            <span className="w" style={{ width: `${(writes / rw) * 100}%` }} />
                          </span>
                          <span className="rw-n">
                            {count(i.reads)} · {count(i.writes)}
                          </span>
                        </span>
                      )}
                    </td>
                    <td className="r ix-size">
                      <span className="size-cell" title={engine === "yugabyte" && i.is_primary_key ? "Primary-key storage is included in the base table; no independent index size is charged" : undefined}>
                        <span className="bar">
                          <span
                            style={{
                              width: `${i.size_bytes == null ? 0 : Math.max(2, (i.size_bytes / biggest) * 100)}%`,
                            }}
                          />
                        </span>
                        {bytes(i.size_bytes)}
                      </span>
                    </td>
                  </tr>
                  {notes.length > 0 && (
                    <tr className="ix-notes">
                      <td colSpan={5}>
                        <div className="ix-notes-inner">
                          {notes.map((n) => (
                            <div key={n.text} className={`ix-note ${n.tone}`} title={n.text}>
                              {n.finding ? (
                                <button type="button" onClick={() => setState({ view: "overview", finding: n.finding! })} title="Open the finding">
                                  {n.text} <ArrowRight size={11} />
                                </button>
                              ) : n.text}
                            </div>
                          ))}
                        </div>
                      </td>
                    </tr>
                  )}
                  {expanded && (
                    <tr className="ix-uses" id={usesId}>
                      <td colSpan={5}>
                        <div className="ix-uses-inner">
                          <div className="ix-uses-head">
                            <span>Queries using this index</span>
                            <span className="muted">{uses.length} captured</span>
                          </div>
                          {uses.map((u) => (
                            <button type="button"
                              key={u.query.id}
                              className="use-row"
                              onClick={() => setState({ view: "queries", query: u.query.id })}
                              title={`${oneLine(u.query.text)}\nOpen in Queries`}
                            >
                              <span className="mono q">{oneLine(u.query.text)}</span>
                              <span className="muted ops">{u.ops.join(", ")}</span>
                              <ArrowRight size={14} className="muted use-arrow" />
                            </button>
                          ))}
                        </div>
                      </td>
                    </tr>
                  )}
                </Fragment>
              );
            })}
          </tbody>
        </table>
      </div>
      <p className="td-foot">
        “Used by” comes from the plans of the {planned} heaviest queries;{" "}
        {hasUsage
          ? `reads and writes count all activity ${sinceText}.`
          : "Index read and write counters are unavailable in this scan."}
      </p>
    </>
  );
}
