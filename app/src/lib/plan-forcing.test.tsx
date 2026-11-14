import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { PlanForcingStatus } from "../components/PlanForcingStatus";

test("unmeasured forcing stays explicit without offering a plan action", () => {
  const markup = renderToStaticMarkup(<PlanForcingStatus isForced={null} />);
  expect(markup).toContain("Plan forcing status unavailable");
  expect(markup).not.toContain("not forced");
  expect(markup).not.toContain("button");
});

test("only native true shows a forced badge and native false needs no unknown warning", () => {
  expect(renderToStaticMarkup(<PlanForcingStatus isForced />)).toContain("forced");
  expect(renderToStaticMarkup(<PlanForcingStatus isForced={false} />)).toBe("");
});
