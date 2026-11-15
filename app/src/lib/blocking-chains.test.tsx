import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { BlockingChains } from "../components/BlockingChains";
import type { LiveState } from "../gen/LiveState";
import { blockingEvidence } from "../test/blocking";
import { t } from "./i18n";

function markup(live: LiveState): string {
  return renderToStaticMarkup(<BlockingChains
    blocking={live.blocking!}
    connection="pg-demo"
    route={live.session_action_route ?? null}
    actions={live.session_actions}
    actionsNote={live.session_actions_note}
    onInterrupt={async () => "signal-sent"}
    onLocate={() => {}}
  />);
}

test("qualified blocker details and actions come from the projected context rather than an independent same-ID row", () => {
  const live = blockingEvidence();
  live.sessions[0]!.sql = "DELETE FROM replacement_request";
  live.sessions[0]!.application = "replacement-program";
  const html = markup(live);
  expect(html).toContain("original_request_30");
  expect(html).toContain("native-login-30@native-host-30");
  expect(html).toContain("native-program-30");
  expect(html).not.toContain("replacement_request");
  expect(html).not.toContain("replacement-program");
  expect(html).toContain(t("ui.blocking_chains.cancel.button"));
  expect(html).toContain(t("ui.blocking_chains.terminate.button"));
});

test("an unqualified head retains native topology without SQL, client details, inferred state or actions", () => {
  const live = blockingEvidence();
  const chain = live.blocking!.chains[0]!;
  chain.head_kind = "unknown";
  chain.head_ms = null;
  chain.transaction_ms = null;
  chain.tree.context = null;
  const html = markup(live);
  expect(html).toContain("block-session\">30");
  expect(html).toContain("original_request_20");
  expect(html).toContain("original_request_10");
  expect(html).toContain(t("ui.blocking_chains.not_visible"));
  for (const original of ["original_request_30", "native-login-30", "native-host-30", "native-program-30"]) {
    expect(html).not.toContain(original);
  }
  expect(html).not.toContain(t("ui.blocking_chains.cancel.button"));
  expect(html).not.toContain(t("ui.blocking_chains.terminate.button"));
  expect(live.sessions[0]!.sql).toBe("SELECT original_request_30");
});

test("a qualified head cannot authorize the context of an unqualified intermediate owner", () => {
  const live = blockingEvidence();
  live.blocking!.chains[0]!.tree.children[0]!.context = null;
  const html = markup(live);
  expect(html).toContain("original_request_30");
  expect(html).toContain("original_request_10");
  expect(html).toContain("block-session\">20");
  for (const original of ["original_request_20", "native-login-20", "native-host-20", "native-program-20"]) {
    expect(html).not.toContain(original);
  }
  expect(html).toContain(t("ui.blocking_chains.cancel.button"));
  expect(html).toContain(t("ui.blocking_chains.terminate.button"));
});

test("a qualified sleeping owner without transaction evidence retains its native state", () => {
  const live = blockingEvidence();
  const chain = live.blocking!.chains[0]!;
  chain.head_kind = "idle";
  chain.transaction_ms = null;
  chain.tree.context!.state = "sleeping";
  chain.tree.context!.transaction_ms = null;
  chain.tree.context!.action_target!.activity.request = null;
  chain.tree.context!.action_target!.statement = null;
  const html = markup(live);
  expect(html).toContain('<span class="pill head">sleeping</span>');
  expect(html).not.toContain(t("ui.blocking_chains.head.idle_in_transaction"));
  expect(html).not.toContain(t("ui.blocking_chains.cancel.button"));
  expect(html).toContain(t("ui.blocking_chains.terminate.button"));
});

test("qualified SQL and client metadata remain text", () => {
  const live = blockingEvidence();
  const context = live.blocking!.chains[0]!.tree.context!;
  context.sql = "SELECT '<script id=blocking-sql-canary>'";
  context.application = "<img id=blocking-client-canary>";
  const html = markup(live);
  expect(html).toContain("&lt;script id=blocking-sql-canary&gt;");
  expect(html).toContain("&lt;img id=blocking-client-canary&gt;");
  expect(html).not.toContain("<script id=blocking-sql-canary>");
  expect(html).not.toContain("<img id=blocking-client-canary>");
});
