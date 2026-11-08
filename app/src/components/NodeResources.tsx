import { useState } from "react";
import type { MemoryLimit } from "../gen/MemoryLimit";
import type { NodeResources } from "../gen/NodeResources";
import { bytes, percent, when } from "../lib/format";
import { t } from "../lib/i18n";

const PAGE_SIZE = 12;
const memory = (n: number | null) => n === null ? t("ui.node_resources.not_provided") : bytes(n);
const cpu = (n: number | null) => n === null ? t("ui.node_resources.not_provided") : percent(n);
const limit = (n: MemoryLimit | null) => n === null ? t("ui.node_resources.not_provided") : n.kind === "unlimited" ? t("ui.node_resources.unlimited") : bytes(n.bytes);

export function NodeResourcesView({ resources: r }: { resources: NodeResources }) {
  const [page, setPage] = useState(0);
  const nodes = r.nodes.slice(0, 1000).sort((a, b) => Number(a.available) - Number(b.available));
  const currentPage = Math.min(page, Math.max(0, Math.ceil(nodes.length / PAGE_SIZE) - 1));
  const visible = nodes.slice(currentPage * PAGE_SIZE, (currentPage + 1) * PAGE_SIZE);
  return <section className="operational-card" aria-label={t("ui.node_resources.label")}>
    <h4>{t("ui.node_resources.title", { n: nodes.length })}</h4>
    <p className="muted">{r.nodes_expected === null ? t("ui.node_resources.inventory_unknown", { source: r.source }) : t("ui.node_resources.expected", { source: r.source, n: r.nodes_expected })} {r.partial && t("ui.common.read_partial")}</p>
    <p className="muted" title={`${r.observed_from} — ${r.observed_until}`}>{t("ui.node_resources.read_period", { from: when(r.observed_from).short, to: when(r.observed_until).short })}</p>
    {r.notes.map((note, i) => <p className="muted" key={i}>{note}</p>)}
    {!nodes.length ? <p>{t("ui.node_resources.none")}</p> : <div style={{ overflowX: "auto" }}><table className="grid"><thead><tr>
      <th>{t("ui.common.node")}</th><th>{t("ui.node_resources.host_cpu")}</th><th>{t("ui.node_resources.host_memory")}</th><th>{t("ui.node_resources.process_memory")}</th><th>{t("ui.node_resources.soft_limit")}</th><th>{t("ui.node_resources.hard_limit")}</th>
    </tr></thead><tbody>{visible.map((n) => <tr key={n.node}>
      <td className="mono" title={n.node}>{n.node.slice(0, 8)}</td>
      {!n.available ? <td colSpan={5}>{t("ui.node_resources.unavailable")}</td> : <>
        <td>{cpu(n.cpu_user_fraction)} / {cpu(n.cpu_system_fraction)}</td>
        <td>{memory(n.host_memory_available_bytes)} / {memory(n.host_memory_total_bytes)}</td>
        <td title={n.process_memory?.name}>{memory(n.process_memory?.tracked_bytes ?? null)}</td>
        <td>{limit(n.process_memory?.soft_limit ?? null)}</td><td>{limit(n.process_memory?.hard_limit ?? null)}</td>
      </>}
    </tr>)}</tbody></table></div>}
    {nodes.length > PAGE_SIZE && <div className="actions">
      <button type="button" className="btn" disabled={currentPage === 0} onClick={() => setPage(currentPage - 1)}>{t("ui.node_resources.previous")}</button>
      <span className="muted">{t("ui.node_resources.page", { from: String(currentPage * PAGE_SIZE + 1), to: String(Math.min((currentPage + 1) * PAGE_SIZE, nodes.length)), total: String(nodes.length) })}</span>
      <button type="button" className="btn" disabled={(currentPage + 1) * PAGE_SIZE >= nodes.length} onClick={() => setPage(currentPage + 1)}>{t("ui.node_resources.next")}</button>
    </div>}
    {r.nodes.length > nodes.length && <p className="muted">{t("ui.node_resources.truncated", { shown: String(nodes.length), total: String(r.nodes.length) })}</p>}
  </section>;
}
