import type { ObservedWaitResource } from "../gen/ObservedWaitResource";
import { t } from "./i18n";

/** The native identifier stays intact; the label describes a wait target, never its owner. */
export function waitResourceLabel(resource: ObservedWaitResource): string {
  switch (resource.kind) {
    case "memory-page": return t("ui.live.wait_resource.memory_page", { id: resource.native.id });
    case "page-io": return t("ui.live.wait_resource.page_io", { id: resource.native.id });
    case "data-lock": return t("ui.live.wait_resource.data_lock", { id: resource.native.id });
  }
}
