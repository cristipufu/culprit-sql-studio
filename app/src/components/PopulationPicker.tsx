import { Check, ChevronDown, ListFilter } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { RuntimePopulation } from "../gen/RuntimePopulation";
import type { RuntimePopulationCatalog } from "../gen/RuntimePopulationCatalog";
import type { NativeIdentity } from "../gen/NativeIdentity";
import { t } from "../lib/i18n";
import { useHotkeys } from "../lib/hotkeys";
import { nativeIdentityKey } from "../lib/native-identity";
import { samePopulation } from "../lib/report-selection";
import { setPopulation } from "../lib/store";

/** Source labels are rendered by Rust; their native identity and qualifiers still select the exact saved view. */
function label(population: RuntimePopulation): string {
  return population.label.kind === "text" ? population.label.text : t("ui.population_picker.population", { id: population.identity.id });
}

export function PopulationPicker({ connection, catalog, selected, actual = null }: {
  connection: string | null; catalog: RuntimePopulationCatalog | null; selected: NativeIdentity | null; actual?: NativeIdentity | null;
}) {
  const [open, setOpen] = useState(false);
  const anchor = useRef<HTMLDivElement>(null);
  useEffect(() => { setOpen(false); }, [connection]);
  useHotkeys({ Escape: () => setOpen(false) }, open);
  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent) => {
      if (!anchor.current?.contains(event.target as Node)) setOpen(false);
    };
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [open]);
  if (!connection || (!catalog?.partitioned && selected === null)) return null;
  const found = selected && catalog?.populations.find((entry) => samePopulation(entry.identity, selected));
  const observed = actual && catalog?.populations.find((entry) => samePopulation(entry.identity, actual));
  const overview = actual === null ? t("ui.population_picker.default")
    : `${t("ui.population_picker.default")} · ${observed ? label(observed) : t("ui.population_picker.population", { id: actual.id })}`;
  const chosen = selected === null ? overview : found ? label(found) : t("ui.population_picker.population", { id: selected.id });
  const choose = (population: RuntimePopulation | null) => {
    setOpen(false);
    void setPopulation(connection, population?.identity ?? null);
  };
  return (
    <div className="menu-anchor population-picker" ref={anchor}>
      <button type="button" className="picker" aria-label={t("ui.population_picker.label")} aria-haspopup="listbox" aria-expanded={open} title={chosen} onClick={() => setOpen((value) => !value)}>
        <ListFilter size={13} /><span className="population-choice">{chosen}</span><ChevronDown size={13} className="muted" />
      </button>
      {open && <div className="menu population-menu" role="listbox" aria-label={t("ui.population_picker.label")}>
        <button type="button" role="option" aria-selected={selected === null} className={`menu-item ${selected === null ? "on" : ""}`} onClick={() => choose(null)}>
          <span>{t("ui.population_picker.default")}</span>{selected === null && <Check size={13} />}
        </button>
        {catalog?.populations.map((population) => {
          const on = samePopulation(population.identity, selected);
          const context = Object.entries(population.identity.qualifiers).map(([name, value]) => `${name}: ${value}`).join("\n");
          return <button type="button" role="option" key={nativeIdentityKey(population.identity)} aria-selected={on} title={context || label(population)} className={`menu-item ${on ? "on" : ""}`} onClick={() => choose(population)}>
            <span className="population-option"><span>{label(population)}</span><code className="menu-sub">{population.identity.id}</code></span>{on && <Check size={13} />}
          </button>;
        })}
        {selected !== null && !found && <p className="menu-hint">{t("ui.population_picker.unavailable")}</p>}
        <p className="menu-hint">{catalog?.requires_selection && selected === null ? t("ui.population_picker.required") : t("ui.population_picker.hint")}</p>
      </div>}
    </div>
  );
}
