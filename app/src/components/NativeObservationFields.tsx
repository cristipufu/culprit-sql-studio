import type { ReactNode } from "react";
import type { NativeIdentity } from "../gen/NativeIdentity";
import type { CollectionDescription } from "../gen/CollectionDescription";
import { contextPreview } from "../lib/context-preview";
import { t } from "../lib/i18n";

export function nativeTextPreview(value: string, limit: number): string {
  return contextPreview(value.replace(/\p{Cc}/gu, " "), limit).text;
}

export function queryHistoryReadTimes(report: { description: CollectionDescription | null; collected_from: string | null; collected_until: string | null }): ReactNode {
  const history = report.description?.observation;
  const times: Array<{ key: string; text: string }> = [];
  if (history?.kind === "query-history") times.push({ key: "window", text: t("view.query_wait_fragments.window", { from: history.data.interval.from, until: history.data.interval.until }) });
  if (report.collected_from && report.collected_until) times.push({ key: "collection", text: t("ui.memory_failures.read_between", { from: report.collected_from, until: report.collected_until }) });
  return times.length > 0 ? times.map((line) => <span key={line.key} style={{ display: "block" }}>{line.text}</span>) : undefined;
}

export function NativeValue({ label, value }: { label: string; value: ReactNode }) {
  return <div><dt>{label}</dt><dd>{value ?? t("ui.memory_failures.not_reported")}</dd></div>;
}

export function NativeReference({ identity }: { identity: NativeIdentity }) {
  return <><code>{identity.id}</code><NativeProperties properties={identity.qualifiers} /></>;
}

export function NativeProperties({ properties }: { properties: Record<string, string> }) {
  const entries = Object.entries(properties);
  return entries.length > 0 && <details>
    <summary>{t("ui.memory_participants.native_properties")}</summary>
    <dl>{entries.map(([name, value]) => <div key={name}><dt><code>{name}</code></dt><dd><code>{value}</code></dd></div>)}</dl>
  </details>;
}
