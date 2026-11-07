import { expect, test } from "bun:test";
import { EditorTabStorage, MAX_EDITOR_TABS, editorScope, newEditorTab, removeEditorTabs, renameEditorTabs, tabTitle, type EditorTabs } from "./editor-tabs";

test("Editor tabs: scoped restore, exact SQL, incremental writes, rename, removal, bounded storage, corruption and quotas", () => {
  function check(ok: unknown, message: string): asserts ok { if (!ok) throw new Error(message); }
  class MemoryStorage {
    values = new Map<string, string>();
    writes: string[] = [];
    fail: string | null = null;
    getItem(key: string) { return this.values.get(key) ?? null; }
    setItem(key: string, value: string) {
      if (this.fail === "all" || key === this.fail) throw new Error("Quota exceeded");
      this.writes.push(key); this.values.set(key, value);
    }
    removeItem(key: string) { this.values.delete(key); }
  }
  const a = { name: "shop", engine: "postgres" as const, host: "db.example", port: 5432, database: "shop" };
  const b = { ...a, name: "other" };
  const storage = new MemoryStorage();
  const session = new EditorTabStorage(a, "-- Start here", storage);
  check(session.initial.tabs.length === 1 && session.initial.tabs[0]!.sql === "-- Start here", "A new connection starts with one tab");
  const first = newEditorTab("SELECT '😀';\n-- keep whitespace  ", "Scratch");
  const second = newEditorTab("SELECT 9007199254740993", "Revenue", "Revenue");
  const tabs: EditorTabs = { tabs: [first, second], active: second.id };
  check(session.write(tabs) === null, "Valid tabs should save");
  const restored = new EditorTabStorage(a, "", storage).initial;
  check(restored.active === second.id && restored.tabs.length === 2 && restored.tabs.every((t, i) => { const expected = tabs.tabs[i]!; return t.id === expected.id && t.sql === expected.sql && t.title === expected.title && t.saved === expected.saved; }), "SQL, order, selection and saved links must restore exactly");
  check(new EditorTabStorage(b, "", storage).initial.tabs[0]!.sql === "", "Connection aliases must not share tabs");
  check(new EditorTabStorage({ ...a, host: "different.example" }, "", storage).initial.tabs[0]!.sql === "", "Reusing an alias for another database must not restore unrelated SQL");
  check(editorScope(a) === editorScope({ ...a, ssh: { destination: "bastion" } } as typeof a), "Transport settings must not change the database workspace");
  storage.writes = [];
  const edited = { ...tabs, tabs: [first, { ...second, sql: "SELECT 'latest'" }] };
  check(session.write(edited) === null && storage.writes.length === 1 && storage.writes[0]!.endsWith(second.id), "Typing must write only the changed body, not the index or other SQL");
  check(new EditorTabStorage(a, "", storage).initial.tabs[1]!.sql === "SELECT 'latest'", "The last keystroke must survive an immediate restart");
  storage.writes = [];
  check(session.write({ ...edited, active: first.id }) === null && storage.writes.length === 1 && storage.writes[0] === editorScope(a), "Activating a tab must write only metadata");
  check(renameEditorTabs(a, { ...a, name: "renamed" }, storage) === null, "Renaming should move the workspace");
  check(storage.getItem(editorScope(a)) === null && new EditorTabStorage({ ...a, name: "renamed" }, "", storage).initial.tabs[1]!.sql === "SELECT 'latest'", "Rename must preserve the latest SQL without copying bodies");
  check(![...storage.values.values()].some((text) => text.includes('"results"')), "Query output must not be persisted");

  const renamed = { ...a, name: "renamed" };
  const closer = new EditorTabStorage(renamed, "", storage);
  const closed = { tabs: [first], active: first.id };
  storage.fail = editorScope(renamed);
  check(closer.write(closed) !== null && storage.getItem("culprit.editor.sql:" + second.id) !== null, "Failed metadata writes must not delete bodies still referenced by the saved index");
  const extra = newEditorTab("SELECT 'not indexed'");
  check(closer.write({ tabs: [first, second, extra], active: extra.id }) !== null && storage.getItem("culprit.editor.sql:" + extra.id) === null, "Failed index writes must not leak new SQL bodies");
  storage.fail = null;
  check(closer.write(closed) === null && storage.getItem("culprit.editor.sql:" + second.id) === null, "Closed bodies should be retired after saving the index");
  const tooMany = Array.from({ length: MAX_EDITOR_TABS + 1 }, () => newEditorTab());
  check(closer.write({ tabs: tooMany, active: tooMany[0]!.id }) !== null, "Tab counts must be bounded");
  const huge = newEditorTab("x".repeat(2 * 1024 * 1024 + 1));
  check(closer.write({ tabs: [huge], active: huge.id }) !== null && new EditorTabStorage(renamed, "", storage).initial.tabs[0]!.sql === first.sql, "Oversized drafts must not destroy the last saved workspace");

  for (const raw of ["{", "null", JSON.stringify({ version: 1, tabs: [] }), JSON.stringify({ version: 1, tabs: [null] }), JSON.stringify({ version: 1, tabs: [first, first], active: first.id }), JSON.stringify({ version: 1, tabs: [{ ...first, title: null }] }), JSON.stringify({ version: 7, tabs: [first] }), JSON.stringify({ version: 1, tabs: [{ ...first, id: "../../bad" }] }), " ".repeat(64 * 1024 + 1)]) {
    const broken = new MemoryStorage(); broken.setItem(editorScope(a), raw);
    const opened = new EditorTabStorage(a, "-- New scratch", broken);
    check(opened.error && opened.initial.tabs[0]!.sql === "-- New scratch", "Malformed data must fall back without crashing");
    opened.write(opened.initial);
    check(broken.getItem(editorScope(a)) === raw, "Malformed stored copies must remain available for recovery");
  }
  const missing = new MemoryStorage();
  missing.setItem(editorScope(a), JSON.stringify({ version: 1, tabs: [{ id: first.id, title: "Missing", saved: null }], active: first.id }));
  check(new EditorTabStorage(a, "", missing).error, "Missing SQL bodies must not silently restore blank text");
  missing.setItem("culprit.editor.sql:" + first.id, "SELECT 1");
  missing.setItem(editorScope(a), JSON.stringify({ version: 1, tabs: [{ id: first.id, title: "Valid", saved: null }], active: "missing" }));
  check(new EditorTabStorage(a, "", missing).initial.active === first.id, "An invalid active ID must select the first valid tab");

  check(new EditorTabStorage(a, "", null).write(tabs), "Unavailable storage must return a visible error");
  const renaming = new MemoryStorage();
  check(new EditorTabStorage(a, "", renaming).write(tabs) === null, "A workspace should save");
  renaming.fail = editorScope(b);
  check(renameEditorTabs(a, b, renaming) && renaming.getItem(editorScope(a)) !== null, "Failed rename must retain the original workspace");
  const leaving = new MemoryStorage();
  const stays = { ...a, name: "stays" };
  check(new EditorTabStorage(a, "", leaving).write(tabs) === null && new EditorTabStorage(stays, "", leaving).write({ tabs: [extra], active: extra.id }) === null, "Two workspaces should save");
  check(removeEditorTabs(a, leaving) === null && leaving.getItem(editorScope(a)) === null && tabs.tabs.every((t) => leaving.getItem("culprit.editor.sql:" + t.id) === null), "Removing a connection must clear its index and every SQL body");
  check(new EditorTabStorage(a, "", leaving).initial.tabs[0]!.sql === "", "A connection added again at the same endpoint must start with a fresh tab");
  check(new EditorTabStorage(stays, "", leaving).initial.tabs[0]!.sql === extra.sql, "Removing one connection must keep other connections' tabs");
  const unreadable = new MemoryStorage(); unreadable.setItem(editorScope(a), "{");
  check(removeEditorTabs(a, unreadable) === null && unreadable.getItem(editorScope(a)) === null, "An unreadable index must still be removed");
  check(removeEditorTabs(b, new MemoryStorage()) === null && removeEditorTabs(a, null) === null, "Nothing to remove is not an error");
  const denied = { getItem(): string | null { throw new Error("denied"); }, setItem() {}, removeItem() {} };
  check(removeEditorTabs(a, denied), "A storage failure while removing must be reported");
});

test("tab titles come from the first words, skipping comments", () => {
  expect(tabTitle("-- top orders\nSELECT id, total FROM orders ORDER BY total DESC", 2)).toBe("SELECT id, total FROM");
  expect(tabTitle("/* report */ SELECT customer_name_with_a_long_suffix, x FROM t", 1)).toBe("SELECT customer_name_with_a…");
  expect(tabTitle("-- nothing yet\n", 3)).toBe("Query 3");
});
