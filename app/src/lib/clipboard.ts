import { copyText } from "./api";
import { t } from "./i18n";
import { toast } from "./store";

/** Copy text and say so; when the clipboard refuses, say that instead of failing silently. True when it copied. */
export async function copyAndTell(text: string, message?: string): Promise<boolean> {
  try {
    await copyText(text);
    if (message) toast(message);
    return true;
  } catch {
    toast(t("ui.clipboard.copy_failed"));
    return false;
  }
}
