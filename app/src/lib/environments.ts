import type { ConnectionInfo } from "../gen/ConnectionInfo";
import type { Environment } from "../gen/Environment";
import { t } from "./i18n";

export const ENVIRONMENT_ORDER: Environment[] = ["production", "staging", "development", "local"];

/** Each environment's name, read in the app's language each time it is shown. */
export const ENVIRONMENT_LABEL: Record<Environment, string> = {
  get production() {
    return t("ui.environments.production");
  },
  get staging() {
    return t("ui.environments.staging");
  },
  get development() {
    return t("ui.environments.development");
  },
  get local() {
    return t("ui.environments.local");
  },
};

export function groupByEnvironment(connections: ConnectionInfo[]): { environment: Environment; items: ConnectionInfo[] }[] {
  return ENVIRONMENT_ORDER.map((environment) => ({
    environment,
    items: connections.filter((c) => c.environment === environment).sort((a, b) => a.name.localeCompare(b.name)),
  })).filter((g) => g.items.length > 0);
}
