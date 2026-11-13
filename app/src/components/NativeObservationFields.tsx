import type { ReactNode } from "react";
import type { NativeIdentity } from "../gen/NativeIdentity";
import { t } from "../lib/i18n";

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
