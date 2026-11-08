import { expect, test } from "bun:test";
import { registerCatalogs, setLanguage, t } from "./i18n";

test("English fallback keeps its punctuation and reading direction inside an RTL language", () => {
  registerCatalogs({ ar: { _meta: { name: "العربية", english_name: "Arabic", plural: "ar", decimal: ".", group: ",", direction: "rtl" }, "ui.common.copy": "نسخ" } });
  try {
    setLanguage("ar");
    expect(t("ui.common.copy")).toBe("نسخ");
    expect(t("ui.sidebar.language")).toBe("\u2066Language\u2069");
  } finally {
    setLanguage("en");
  }
  expect(t("ui.sidebar.language")).toBe("Language");
});
