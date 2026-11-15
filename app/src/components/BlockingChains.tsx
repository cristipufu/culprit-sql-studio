import { ChevronDown, ChevronRight, Lock } from "lucide-react";
import { useLayoutEffect, useRef, useState } from "react";
import type { BlockNode } from "../gen/BlockNode";
import type { Blocking } from "../gen/Blocking";
import type { BlockingChain } from "../gen/BlockingChain";
import type { SessionAction } from "../gen/SessionAction";
import type { SessionActionOutcome } from "../gen/SessionActionOutcome";
import type { SessionActionRequest } from "../gen/SessionActionRequest";
import type { SessionActionRoute } from "../gen/SessionActionRoute";
import { duration, oneLine } from "../lib/format";
import { t } from "../lib/i18n";
import { SessionActionControls } from "./SessionActionControls";

type Props = {
  blocking: Blocking;
  connection: string;
  route: SessionActionRoute | null;
  actions: SessionAction[];
  actionsNote: string | null;
  onInterrupt: (request: SessionActionRequest) => Promise<SessionActionOutcome>;
  onLocate: (session: string) => void;
};

function headLabel(kind: BlockingChain["head_kind"]): string {
  switch (kind) {
    case "idle-in-transaction":
      return t("ui.blocking_chains.head.idle_in_transaction");
    case "running":
      return t("ui.blocking_chains.head.running");
    case "idle":
      return t("ui.blocking_chains.head.idle");
    case "unknown":
      return t("ui.blocking_chains.not_visible");
  }
}

/** Every chain, biggest first: a calm card with the head blocker, the story, and the tree of who waits behind whom. */
export function BlockingChains({ blocking, connection, route, actions, actionsNote, onInterrupt, onLocate }: Props) {
  return (
    <div className="block-chains" role="group" aria-label={t("ui.blocking_chains.label", { count: blocking.waiting })}>
      {blocking.chains.map((chain, i) => (
        <ChainCard key={chain.head} chain={chain} connection={connection} route={route} actions={actions} actionsNote={actionsNote} onInterrupt={onInterrupt} onLocate={onLocate} defaultOpen={i < 2} />
      ))}
    </div>
  );
}

function ChainCard({ chain, connection, route, actions, actionsNote, onInterrupt, onLocate, defaultOpen }: Omit<Props, "blocking"> & { chain: BlockingChain; defaultOpen: boolean }) {
  const [open, setOpen] = useState(defaultOpen);
  const [adviceOpen, setAdviceOpen] = useState(false);
  const [clamped, setClamped] = useState(false);
  const adviceRef = useRef<HTMLParagraphElement>(null);
  useLayoutEffect(() => {
    const el = adviceRef.current;
    if (el && !adviceOpen) setClamped(el.scrollHeight > el.clientHeight + 1);
  }, [chain.advice, adviceOpen]);
  const head = chain.tree.context;
  const severity = chain.waiters >= 5 || (chain.longest_wait_ms ?? 0) > 60_000 ? "high" : "medium";
  const treeRef = useRef<HTMLDivElement>(null);

  const moveFocus = (e: React.KeyboardEvent) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    const rows = [...(treeRef.current?.querySelectorAll<HTMLElement>("[role=treeitem]") ?? [])];
    const index = rows.indexOf(document.activeElement as HTMLElement);
    const next = rows[Math.min(rows.length - 1, Math.max(0, index + (e.key === "ArrowDown" ? 1 : -1)))];
    if (next) {
      e.preventDefault();
      next.focus();
    }
  };

  return (
    <section className={`block-card ${chain.head_kind}`} aria-label={chain.headline}>
      <header className="block-head">
        <span className={`sev ${severity}`} aria-hidden />
        <h3>{chain.headline}</h3>
      </header>
      <div className="block-facts">
        <span className="fact">{headLabel(chain.head_kind)}</span>
        {chain.transaction_ms != null && (
          <span className="fact">
            <span className="fact-label">{t("ui.blocking_chains.transaction_open")}</span> {duration(chain.transaction_ms)}
          </span>
        )}
        <span className="fact">
          <span className="fact-label">{t("ui.blocking_chains.waiting")}</span> {t("ui.blocking_chains.sessions", { n: chain.waiters })}
        </span>
        {chain.longest_wait_ms != null && (
          <span className="fact">
            <span className="fact-label">{t("ui.blocking_chains.longest_wait")}</span> {duration(chain.longest_wait_ms)}
          </span>
        )}
        {chain.wait_type && <code className="fact-code" title={t("ui.blocking_chains.wait_type")}>{chain.wait_type}</code>}
      </div>
      <p className={`block-advice ${adviceOpen ? "open" : ""}`} ref={adviceRef}>
        {chain.advice}{" "}
      </p>
      {!adviceOpen && clamped && (
        <button type="button" className="link-button block-more" onClick={() => setAdviceOpen(true)} aria-label={t("ui.blocking_chains.more_label")}>
          {t("ui.blocking_chains.more")}
        </button>
      )}
      <div className="block-tree-head">
        <button type="button" className="link-button" aria-expanded={open} onClick={() => setOpen((o) => !o)}>
          {open ? <ChevronDown size={13} /> : <ChevronRight size={13} />} {open ? t("ui.blocking_chains.hide_tree") : t("ui.blocking_chains.show_tree")}
        </button>
        <span className="spacer" />
        {head && actions.length > 0 && <SessionActionControls connection={connection} route={route} session={head} actions={actions} variant="blocker" execute={onInterrupt} />}
        {actions.length === 0 && actionsNote && (
          <span className="muted block-note" title={actionsNote}>
            <Lock size={11} /> {t("ui.blocking_chains.production")}
          </span>
        )}
      </div>
      {open && (
        <div className="block-tree" role="tree" ref={treeRef} onKeyDown={moveFocus}>
          <TreeRow node={chain.tree} depth={0} onLocate={onLocate} isHead />
        </div>
      )}
    </section>
  );
}

function TreeRow({ node, depth, onLocate, isHead = false }: { node: BlockNode; depth: number; onLocate: (id: string) => void; isHead?: boolean }) {
  const s = node.context;
  const who = s ? [s.user && s.host ? `${s.user}@${s.host}` : (s.user ?? s.host), s.application].filter(Boolean).join(" · ") : t("ui.blocking_chains.not_visible");
  const waited = s?.wait_ms ?? s?.elapsed_ms ?? null;
  return (
    <>
      <div
        className={`block-row ${isHead ? "head" : ""}`}
        role="treeitem"
        aria-level={depth + 1}
        aria-expanded={node.children.length ? true : undefined}
        tabIndex={0}
        style={{ ["--depth" as string]: depth }}
        onClick={s ? () => onLocate(s.id) : undefined}
        onKeyDown={s ? (e) => e.key === "Enter" && onLocate(s.id) : undefined}
        title={s ? t("ui.blocking_chains.locate") : undefined}
      >
        <span className="block-indent" aria-hidden />
        <span className="block-session">{node.session}</span>
        <span className="block-state">
          {s && (isHead ? (
            <span className="pill head">{s.state}</span>
          ) : (
            <span className="pill waiting">{t("ui.blocking_chains.waits", { duration: waited != null ? duration(waited) : "" })}</span>
          ))}
        </span>
        <span className="block-what">
          <span className="block-who">{who}</span>
          {node.waits_for && <span className="block-lock">{t("ui.blocking_chains.wants", { lock: node.waits_for })}</span>}
          {s?.sql && <span className="block-sql mono">{oneLine(s.sql)}</span>}
        </span>
      </div>
      {node.children.map((c) => (
        <TreeRow key={c.session} node={c} depth={depth + 1} onLocate={onLocate} />
      ))}
    </>
  );
}
