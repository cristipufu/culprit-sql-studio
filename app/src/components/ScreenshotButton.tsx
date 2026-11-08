import { Camera } from "lucide-react";
import { t } from "../lib/i18n";
import { canScreenshot, screenshotWorkspace } from "../lib/screenshot";

/** Top-bar button: the workspace as a PNG, without the sidebar, on the clipboard. */
export function ScreenshotButton() {
  if (!canScreenshot()) return null;
  return (
    <button type="button"
      className="btn ghost icon shot-btn"
      onClick={() => void screenshotWorkspace()}
      title={t("ui.screenshot_button.title")}
      aria-label={t("ui.screenshot_button.label")}
    >
      <Camera size={14} />
    </button>
  );
}
