import type { Environment } from "../../gen/Environment";
import { t } from "../../lib/i18n";

const ENVIRONMENTS: Environment[] = ["production", "staging", "development", "local"];

export function EnvironmentPicker({ value, onChange, disabled = false, labelledBy }: { value: Environment; onChange: (value: Environment) => void; disabled?: boolean; labelledBy: string }) {
  return <div className="seg connection-environments" role="radiogroup" aria-labelledby={labelledBy}>
    {ENVIRONMENTS.map((environment) => <button key={environment} type="button" role="radio" aria-checked={value === environment} className={value === environment ? "on" : ""} disabled={disabled} onClick={() => onChange(environment)}>
      <span className={`env-dot env-${environment}`} aria-hidden />{t("ui.common.environment", { environment })}
    </button>)}
  </div>;
}
