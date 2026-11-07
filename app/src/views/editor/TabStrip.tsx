import { Bookmark, PanelLeftClose, PanelLeftOpen, Plus, X } from "lucide-react";
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

/** The editor's tabs, with the saved-queries rail toggle in front and a new-tab button after them. */
export function TabStrip({ tabs, activeId, dirty, production, railOpen, onToggleRail, onActivate, onClose, onNew }: Props) {
  const full = tabs.length >= MAX_EDITOR_TABS;
  return (
    <div className={`editor-tabs ${production ? "is-production" : ""}`} role="tablist">
      <button className="icon-btn" title={railOpen ? "Hide saved queries" : "Show saved queries"} onClick={onToggleRail}>
        {railOpen ? <PanelLeftClose size={14} /> : <PanelLeftOpen size={14} />}
      </button>
      <div className="tab-strip">
        {tabs.map((tab) => (
          <div
            key={tab.id}
            role="tab"
            aria-selected={tab.id === activeId}
            className={`editor-tab ${tab.id === activeId ? "on" : ""}`}
            onClick={() => onActivate(tab.id)}
            onAuxClick={(e) => e.button === 1 && onClose(tab.id)}
            title={tab.saved ? `Saved query “${tab.saved}”` : tab.title}
          >
            {tab.saved && <Bookmark size={11} className="tab-saved" />}
            <span className="tab-title">{tab.title}</span>
            {dirty(tab) && <span className="tab-dirty" title="Changed since it was saved" />}
            <button className="tab-close" title={keys("Close tab (⌘⇧W)")} onClick={(e) => (e.stopPropagation(), onClose(tab.id))}>
              <X size={11} />
            </button>
          </div>
        ))}
        <button className="icon-btn" title={full ? "Close a tab before opening another" : keys("New tab (⌘T)")} disabled={full} onClick={onNew}>
          <Plus size={14} />
        </button>
      </div>
    </div>
  );
}
