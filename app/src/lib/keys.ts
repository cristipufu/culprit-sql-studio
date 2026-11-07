import type { KeyboardEvent } from "react";

/**
 * Shortcut labels are written the Mac way ("⌘⇧E"). On Windows and Linux they read "Ctrl+Shift+E"; the handlers accept
 * either modifier (lib/hotkeys.ts, CodeMirror's Mod-), so only the label changes.
 */
export const isMac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.userAgent);

const NAMES: Record<string, string> = { "⌘": "Ctrl+", "⇧": "Shift+", "⌥": "Alt+", "↵": "Enter" };

/** Any text with Mac shortcut symbols in it ("Press ⌘V to paste"), in this platform's words. */
export function keys(mac: string): string {
  return isMac ? mac : mac.replace(/[⌘⇧⌥↵]/g, (k) => NAMES[k] ?? k);
}

const TAB_MOVES: Record<string, (at: number, last: number) => number> = {
  ArrowRight: (at, last) => (at === last ? 0 : at + 1),
  ArrowLeft: (at, last) => (at === 0 ? last : at - 1),
  Home: () => 0,
  End: (_, last) => last,
};

/**
 * A tab list's keys, as the WAI-ARIA tabs pattern has them: the arrow keys move to the next or previous tab, Home and
 * End to the first and last, and the tab reached is chosen by its own click handler. With `tabIndex` 0 on the chosen
 * tab and -1 on the others, Tab then leaves the list instead of walking every tab.
 */
export function tabListKeys(e: KeyboardEvent<HTMLElement>) {
  const tabs = [...e.currentTarget.querySelectorAll<HTMLElement>('[role="tab"]:not(:disabled)')];
  const at = tabs.findIndex((t) => t.contains(document.activeElement));
  const move = TAB_MOVES[e.key];
  if (at < 0 || !move) return;
  // Handled here: the view's own arrow-key shortcuts don't also see it.
  e.preventDefault();
  e.stopPropagation();
  const next = tabs[move(at, tabs.length - 1)];
  next?.focus();
  next?.click();
}
