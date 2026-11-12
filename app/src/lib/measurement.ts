import type { ExecutionProtection } from "../gen/ExecutionProtection";
import { t } from "./i18n";

/** Only the native execution's confirmed facts can establish how the statement was protected. */
export function measurementOrigin(protection: ExecutionProtection | null | undefined): string {
  switch (protection) {
    case "read-only-transaction": return t("ui.actual_run.measured_read_only");
    case "rolled-back-transaction": return t("ui.actual_run.measured_rolled_back");
    default: return t("ui.actual_run.measured");
  }
}
