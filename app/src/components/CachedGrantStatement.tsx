import type { Snapshot } from "../gen/Snapshot";
import type { StatementGrantReference } from "../gen/StatementGrantReference";
import { t } from "../lib/i18n";
import { originalStatementGrant } from "../lib/statement-grant-reference";
import { Sql } from "./Sql";

/** Original cache-entry text is read-only evidence, never a fabricated workload query or an execution action. */
export function CachedGrantStatement({ snapshot, reference }: { snapshot: Snapshot; reference: StatementGrantReference }) {
  const original = originalStatementGrant(snapshot, reference);
  if (!original) return <section aria-label={t("ui.statement_grants.evidence")}>
    <h3>{t("ui.statement_grants.evidence")}</h3>
    <p className="muted">{t("ui.statement_grants.missing")}</p>
  </section>;
  const { read, row } = original;
  const observation = read.description.observation;
  const partition = read.description.partition;
  if (observation.kind !== "snapshot" || partition.kind !== "instance-local") return null;
  return <section aria-label={t("ui.statement_grants.evidence")}>
    <h3>{t("ui.statement_grants.evidence")}</h3>
    <p className="muted">{t("ui.statement_grants.read", { source: read.description.source, from: observation.data.observed_from, until: observation.data.observed_until })}</p>
    {partition.member && <p className="muted">{t("ui.statement_grants.member", { member: partition.member.native.id })}</p>}
    {(!partition.member || !partition.epoch) && <p className="muted">{t("view.statement_grants.limit.owner")}</p>}
    {row.statement.state === "available"
      ? <div className="codeblock"><Sql text={row.statement.text} pretty={false} /></div>
      : <p className="muted">{t("ui.statement_grants.unavailable", { reason: row.statement.reason })}</p>}
  </section>;
}
