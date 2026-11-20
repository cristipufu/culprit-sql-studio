import type { TableFullTextEntry } from "../../gen/TableFullTextEntry";
import type { TableFullTextReport } from "../../gen/TableFullTextReport";

/** Only the generated catalog explanation is rendered; raw flags never supply an independent verdict. */
export function FullText({ report, entry }: { report: TableFullTextReport; entry: TableFullTextEntry }) {
  return <details className="td-chunks td-catalog">
    <summary>{report.title}</summary>
    <p>{entry.summary}</p>
    <dl>{entry.metrics.map((metric) => <div key={metric.key}><dt>{metric.label}</dt><dd>{metric.text}</dd></div>)}</dl>
    {report.limitations.map((limitation, index) => <p key={index} className="muted">{limitation}</p>)}
    <p className="muted">{entry.qualification}</p>
    <p className="muted">{entry.verify}</p>
  </details>;
}
