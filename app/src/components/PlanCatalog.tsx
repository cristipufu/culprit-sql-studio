import type { DatabaseProduct } from "../gen/DatabaseProduct";
import { PRODUCTS } from "../lib/engines";
import { t } from "../lib/i18n";
import { EngineBadge } from "./EngineBadge";

/** Formats and sample actions follow the generated core catalog together. */
export function PlanCatalog({ onSample }: { onSample: (product: DatabaseProduct) => void }) {
  return (
    <>
      <div className="formats">
        {PRODUCTS.map((product) => (
          <div className="format-row" key={product.id} data-product={product.id}>
            <b><EngineBadge engine={product.engine} product={product.prefix} />{product.label}</b>
            <span className="muted">{t(product.plan_format)}</span>
          </div>
        ))}
      </div>
      <div className="welcome-samples">
        <span className="muted">{t("ui.plan_catalog.samples")}</span>
        {PRODUCTS.map((product) => (
          <button type="button" key={product.id} className="btn ghost small" data-product={product.id} onClick={() => onSample(product.id)}>
            <EngineBadge engine={product.engine} product={product.prefix} />
            {product.label}
          </button>
        ))}
      </div>
    </>
  );
}
