import type { PlanVariant } from "../gen/PlanVariant";
import { t } from "../lib/i18n";

export function PlanForcingStatus({ isForced }: { isForced: PlanVariant["is_forced"] }) {
  if (isForced === false) return null;
  return <span className={`tag${isForced === true ? " medium" : ""}`} style={{ marginLeft: 6 }}>{isForced === true ? t("ui.queries.forced") : t("ui.queries.forcing_unknown")}</span>;
}
