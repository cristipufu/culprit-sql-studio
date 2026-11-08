import type { TileStatus } from "../gen/TileStatus";
import { t } from "./i18n";

/** What each tile status says, read in the app's language each time it is shown. */
export const STATUS_LABEL: Record<TileStatus, string> = {
  get bad() {
    return t("ui.dashboard.status.bad");
  },
  get watch() {
    return t("ui.dashboard.status.watch");
  },
  get good() {
    return t("ui.dashboard.status.good");
  },
  get info() {
    return t("ui.dashboard.status.info");
  },
  get unavailable() {
    return t("ui.dashboard.status.unavailable");
  },
};
