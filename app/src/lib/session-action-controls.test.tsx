import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { SessionActionControls } from "../components/SessionActionControls";
import type { SessionActionRoute } from "../gen/SessionActionRoute";
import { blockingEvidence } from "../test/blocking";
import { sessionActionRoute } from "../test/session-actions";
import { t } from "./i18n";

function markup(route: SessionActionRoute | null, withTarget = true, withRequest = true): string {
  const session = blockingEvidence().sessions[0]!;
  if (!withTarget) delete session.action_target;
  if (!withRequest) {
    session.action_target!.activity.request = null;
    session.action_target!.statement = null;
  }
  return renderToStaticMarkup(<SessionActionControls connection="pg-demo" route={route} session={session} actions={["cancel", "terminate"]} variant="row" execute={async () => "signal-sent"} />);
}

test("shared controls offer only actions supported by the native target and exact observed route", () => {
  const html = markup(sessionActionRoute());
  expect(html).toContain(t("ui.common.cancel"));
  expect(html).toContain(t("ui.live.action.end"));
  for (const html of [markup(null), markup(sessionActionRoute("other-connection")), markup(sessionActionRoute(), false)]) {
    expect(html).not.toContain("<button");
  }
});

test("a visible active state cannot create a request cancellation target", () => {
  const html = markup(sessionActionRoute(), true, false);
  expect(html).not.toContain(t("ui.common.cancel"));
  expect(html).toContain(t("ui.live.action.end"));
});
