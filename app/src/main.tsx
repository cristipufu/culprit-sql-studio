import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./styles.css";
import { App } from "./App";
import { loadCatalogs } from "./lib/catalogs";
import { language, subscribe } from "./lib/i18n";
import { isMac } from "./lib/keys";

// Every locales/*.json is a language; the page says which one it is in, for screen readers and hyphenation.
loadCatalogs();
const markLanguage = () => {
  document.documentElement.lang = language();
};
markLanguage();
subscribe(markLanguage);

// macOS draws its window buttons over the sidebar (titleBarStyle "Overlay"); Windows and Linux keep a native title bar.
document.documentElement.dataset.platform = isMac ? "mac" : "other";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
