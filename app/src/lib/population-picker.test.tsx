import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { PopulationPicker } from "../components/PopulationPicker";
import { GROUP_A, GROUP_B, GROUP_B_LABEL, POPULATIONS } from "../test/populations";
import { language, setLanguage } from "./i18n";

test("Query history choices remain available without a default workload and show source labels only as text", () => {
  const previous = language();
  setLanguage("en");
  try {
    const overview = renderToStaticMarkup(<PopulationPicker connection="fixture" catalog={POPULATIONS} selected={null} />);
    expect(overview).toContain('aria-expanded="false"');
    expect(overview).not.toContain('role="listbox"');
    expect(overview).toContain("Overview");
    const singleGroupDefault = renderToStaticMarkup(<PopulationPicker connection="fixture" catalog={POPULATIONS} selected={null} actual={GROUP_A} />);
    expect(singleGroupDefault).toContain("Overview · Write group");
    const selected = renderToStaticMarkup(<PopulationPicker connection="fixture" catalog={POPULATIONS} selected={GROUP_B} />);
    expect(selected).toContain("Read group &lt;img");
    expect(selected).not.toContain("<img");
    expect(selected).not.toContain(GROUP_B_LABEL);
    expect(renderToStaticMarkup(<PopulationPicker connection="fixture" catalog={POPULATIONS} selected={{ ...GROUP_A, qualifiers: { generation: "another-native-context" } }} />)).toContain("Group 17");
  } finally { setLanguage(previous); }
});

test("Undeclared or unpartitioned history does not offer a fabricated execution-group selector", () => {
  for (const catalog of [null, { ...POPULATIONS, partitioned: false, requires_selection: false, populations: [] }]) {
    expect(renderToStaticMarkup(<PopulationPicker connection="fixture" catalog={catalog} selected={null} />)).toBe("");
  }
});
