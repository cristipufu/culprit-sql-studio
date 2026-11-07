import { Bookmark, PanelLeftClose, PanelLeftOpen, Plus, X } from "lucide-react";
import { useRef, useState, type KeyboardEvent } from "react";
import { MAX_EDITOR_TABS, type EditorTab as Tab } from "../../lib/editor-tabs";
import { keys } from "../../lib/keys";

type Props = {
  tabs: Tab[];
  activeId: string;
  /** Whether a saved query's tab has changed since it was saved. */
  dirty: (tab: Tab) => boolean;
  production: boolean;
  railOpen: boolean;
  onToggleRail: () => void;
  onActivate: (id: string) => void;
  onClose: (id: string) => void;
  onNew: () => void;
};

/**
 * The editor's tabs, with the saved-queries rail toggle in front and a new-tab button after them. One tab is in the
 * Tab order; arrow keys, Home and End move between tabs, Enter or Space opens one, Delete closes it.
 */
export function TabStrip({ tabs, activeId, dirty, production, railOpen, onToggleRail, onActivate, onClose, onNew }: Props) {
  const full = tabs.length >= MAX_EDITOR_TABS;
  const strip = useRef<HTMLDivElement>(null);
  const [focused, setFocused] = useState<string | null>(null);
  const reachable = tabs.some((t) => t.id === focused) ? focused : activeId;
  const onKey = (event: KeyboardEvent, index: number, id: string) => {
    const to = { ArrowRight: index + 1, ArrowLeft: index - 1, Home: 0, End: tabs.length - 1 }[event.key];
    if (to !== undefined) {
      event.preventDefault();
      const next = tabs[(to + tabs.length) % tabs.length]!;
      setFocused(next.id);
      strip.current?.querySelector<HTMLElement>(`[data-tab="${CSS.escape(next.id)}"]`)?.focus();
    } else if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      onActivate(id);
    } else if (event.key === "Delete") {
      event.preventDefault();
      onClose(id);
    }
  };
  return (
    <div className={`editor-tabs ${production ? "is-production" : ""}`} role="tablist">
      <button type="button" className="icon-btn" title={railOpen ? "Hide saved queries" : "Show saved queries"} onClick={onToggleRail}>
        {railOpen ? <PanelLeftClose size={14} /> : <PanelLeftOpen size={14} />}
      </button>
      <div className="tab-strip" ref={strip}>
        {tabs.map((tab, index) => (
          <div
            key={tab.id}
            data-tab={tab.id}
            role="tab"
            tabIndex={tab.id === reachable ? 0 : -1}
            aria-selected={tab.id === activeId}
            onKeyDown={(e) => onKey(e, index, tab.id)}
            onFocus={() => setFocused(tab.id)}
            className={`editor-tab ${tab.id === activeId ? "on" : ""}`}
            onClick={() => onActivate(tab.id)}
            onAuxClick={(e) => e.button === 1 && onClose(tab.id)}
            title={tab.saved ? `Saved query “${tab.saved}”` : tab.title}
          >
            {tab.saved && <Bookmark size={11} className="tab-saved" />}
            <span className="tab-title">{tab.title}</span>
            {dirty(tab) && <span className="tab-dirty" title="Changed since it was saved" />}
            {/* Out of the Tab order: Delete closes the focused tab. */}
            <button type="button" className="tab-close" tabIndex={-1} title={keys("Close tab (⌘⇧W)")} onClick={(e) => (e.stopPropagation(), onClose(tab.id))}>
              <X size={11} />
            </button>
          </div>
        ))}
        <button type="button" className="icon-btn" title={full ? "Close a tab before opening another" : keys("New tab (⌘T)")} disabled={full} onClick={onNew}>
          <Plus size={14} />
        </button>
      </div>
    </div>
  );
}
