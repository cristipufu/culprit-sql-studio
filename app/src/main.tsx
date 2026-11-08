import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./styles.css";
import { App } from "./App";
import { loadCatalogs } from "./lib/catalogs";
import { applyApiLanguage } from "./lib/api";
import { language, locale, subscribe } from "./lib/i18n";
import { loadConnections, reread } from "./lib/store";
import { isMac } from "./lib/keys";

// Every locales/*.json is a language; the page says which one it is in, for screen readers and hyphenation.
loadCatalogs();
const markLanguage = () => {
  document.documentElement.lang = language();
  document.documentElement.dir = locale().direction ?? "ltr";
};
markLanguage();
subscribe(markLanguage);
// A new language: the backend words what it reads in it, and the open report is read again.
subscribe(() => {
  const code = language();
  void applyApiLanguage(code)
    .then(() => language() === code ? Promise.allSettled([loadConnections(), reread()]) : undefined, () => {});
});

// macOS draws its window buttons over the sidebar (titleBarStyle "Overlay"); Windows and Linux keep a native title bar.
document.documentElement.dataset.platform = isMac ? "mac" : "other";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
