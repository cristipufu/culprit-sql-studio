import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./styles.css";
import { App } from "./App";
import { loadCatalogs } from "./lib/catalogs";
import { api } from "./lib/api";
import { language, subscribe } from "./lib/i18n";
import { reread } from "./lib/store";
import { isMac } from "./lib/keys";

// Every locales/*.json is a language; the page says which one it is in, for screen readers and hyphenation.
loadCatalogs();
const markLanguage = () => {
  document.documentElement.lang = language();
};
markLanguage();
subscribe(markLanguage);
// A new language: the backend words what it reads in it, and the open report is read again.
subscribe(() => {
  void api()
    .then((client) => client.setLanguage(language()))
    .then(reread, () => {});
});

// macOS draws its window buttons over the sidebar (titleBarStyle "Overlay"); Windows and Linux keep a native title bar.
document.documentElement.dataset.platform = isMac ? "mac" : "other";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
