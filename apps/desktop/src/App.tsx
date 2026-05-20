import {
  Archive,
  Check,
  Clipboard,
  Cloud,
  Copyright,
  Code2,
  Eye,
  Eraser,
  FileText,
  FolderPlus,
  Heading1,
  Info,
  Keyboard,
  List,
  Maximize2,
  Minimize2,
  MonitorCog,
  PanelLeft,
  PanelLeftClose,
  PanelLeftOpen,
  Pin,
  Plus,
  Quote,
  Search,
  Settings,
  Sparkles,
  Tag,
  Trash2,
  X,
} from "lucide-react";
import { lazy, Suspense, type KeyboardEvent, useEffect, useMemo, useRef, useState } from "react";
import type { AppSettings, LocalStats, Memo, Repository, SaveMemoInput } from "./types";
import {
  bootstrap,
  captureClipboardMemo,
  checkShortcuts,
  createRepository,
  currentWindowLabel,
  deleteMemo,
  emitMemosChanged,
  APP_EVENTS,
  isDesktopApp,
  listenCurrentWindowFocus,
  listenAppEvent,
  listenMemosChanged,
  listenSyncCompleted,
  readClipboardText,
  saveMemo,
  saveQuickMemo,
  showQuickCaptureWindow,
  showSettingsWindow,
  syncNow,
  updateAppSettings,
  windowAction,
} from "./tauri";
import { memoSearchText, tokenizeTags } from "./search";

const colors = ["#c86f52", "#6f8f83", "#5f7597", "#9a7a42", "#8a6fa8"];
const MarkdownView = lazy(() => import("./MarkdownView"));
const defaultSettings: AppSettings = {
  server_url: "http://127.0.0.1:7373",
  quick_capture_shortcut: "Ctrl+Shift+KeyM",
  clipboard_capture_shortcut: "Ctrl+Shift+Alt+KeyV",
  settings_shortcut: "Ctrl+Shift+KeyS",
  writing_mode: "split",
  compact_sidebar_on_start: false,
  auto_sync_enabled: true,
  auto_sync_interval_secs: 60,
  realtime_sync_enabled: true,
};

const emptyStats: LocalStats = {
  memo_count: 0,
  repository_count: 0,
  pending_operations: 0,
  last_server_sequence: 0,
};

type Mode = "edit" | "preview" | "split";
type Dialog = "settings" | "shortcuts" | "about" | null;
type CaptureMode = "edit" | "split" | "preview";

export function App() {
  const windowLabel = currentWindowLabel();
  if (isDesktopApp && windowLabel === "quick-capture") {
    return <QuickCaptureWindow />;
  }
  if (isDesktopApp && windowLabel === "settings") {
    return <SettingsWindow />;
  }
  return <WorkbenchApp />;
}

function WorkbenchApp() {
  const [repositories, setRepositories] = useState<Repository[]>([]);
  const [memos, setMemos] = useState<Memo[]>([]);
  const [activeRepo, setActiveRepo] = useState<string | "all">("all");
  const [activeMemoId, setActiveMemoId] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [tagFilter, setTagFilter] = useState<string | null>(null);
  const [mode, setMode] = useState<Mode>("split");
  const [quickOpen, setQuickOpen] = useState(false);
  const [quickText, setQuickText] = useState("");
  const [quickRepo, setQuickRepo] = useState<string>("");
  const [newRepoOpen, setNewRepoOpen] = useState(false);
  const [serverUrl, setServerUrl] = useState("http://127.0.0.1:7373");
  const [settings, setSettings] = useState<AppSettings>(defaultSettings);
  const [syncText, setSyncText] = useState("Idle");
  const [localStats, setLocalStats] = useState<LocalStats>(emptyStats);
  const [deviceId, setDeviceId] = useState("");
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [dialog, setDialog] = useState<Dialog>(null);
  const [saveText, setSaveText] = useState("Saved");
  const quickRepoRef = useRef("");
  const repositoriesRef = useRef<Repository[]>([]);
  const saveTimerRef = useRef<number | null>(null);
  const pendingSaveRef = useRef<SaveMemoInput | null>(null);

  useEffect(() => {
    quickRepoRef.current = quickRepo;
  }, [quickRepo]);

  useEffect(() => {
    repositoriesRef.current = repositories;
  }, [repositories]);

  useEffect(() => {
    bootstrap().then(applyBootstrap);
    const unsubs: Array<() => void> = [];
    listenAppEvent(APP_EVENTS.openQuickCapture, () => {
      if (!isDesktopApp) setQuickOpen(true);
    }).then((unsub) => unsubs.push(unsub));
    listenAppEvent(APP_EVENTS.clipboardCaptureRequested, async () => {
      if (isDesktopApp) return;
      const repoId = quickRepoRef.current || repositoriesRef.current[0]?.id || "";
      if (repoId) setQuickRepo(repoId);
      setQuickOpen(true);
      setQuickText(await readClipboardText());
    }).then((unsub) => unsubs.push(unsub));
    listenMemosChanged(async (payload) => {
      const refreshed = await bootstrap();
      applyBootstrap(refreshed, payload.active_memo_id ?? null);
    }).then((unsub) => unsubs.push(unsub));
    listenSyncCompleted((payload) => {
      if (payload.ok) {
        setSyncText(
          payload.background
            ? `Auto: pushed ${payload.pushed}, pulled ${payload.pulled}`
            : `Pushed ${payload.pushed}, pulled ${payload.pulled}`,
        );
        void bootstrap().then((data) => setLocalStats(data.local_stats));
      } else if (payload.background) {
        setSyncText(`Auto sync: ${payload.message}`);
      }
    }).then((unsub) => unsubs.push(unsub));
    return () => {
      if (saveTimerRef.current !== null) window.clearTimeout(saveTimerRef.current);
      if (pendingSaveRef.current) void saveMemo(pendingSaveRef.current);
      unsubs.forEach((unsub) => unsub());
    };
  }, []);

  function applyBootstrap(data: Awaited<ReturnType<typeof bootstrap>>, preferredMemoId?: string | null) {
    setRepositories(data.repositories);
    setMemos(data.memos);
    setDeviceId(data.device_id);
    setSettings(data.settings);
    setLocalStats(data.local_stats);
    setServerUrl(data.settings.server_url);
    setMode(data.settings.writing_mode);
    setSidebarCollapsed(data.settings.compact_sidebar_on_start);
    setQuickRepo((current) => current || data.repositories[0]?.id || "");
    if (preferredMemoId) {
      setActiveRepo("all");
      setTagFilter(null);
      setQuery("");
      setActiveMemoId(preferredMemoId);
    } else {
      setActiveMemoId((current) => current ?? data.memos[0]?.id ?? null);
    }
  }

  const visibleMemos = useMemo(() => {
    const lower = query.trim().toLowerCase();
    return memos.filter((memo) => {
      if (memo.deleted) return false;
      if (activeRepo !== "all" && memo.repository_id !== activeRepo) return false;
      if (tagFilter && !memo.tags.includes(tagFilter)) return false;
      if (!lower) return true;
      return memoSearchText(memo).includes(lower);
    });
  }, [activeRepo, memos, query, tagFilter]);

  const activeMemo = visibleMemos.find((memo) => memo.id === activeMemoId) ?? visibleMemos[0] ?? null;
  const tags = [...new Set(memos.flatMap((memo) => memo.tags))].sort();
  const activeRepository = repositories.find((repo) => repo.id === activeMemo?.repository_id);
  const captureRepoId = activeRepo !== "all" ? activeRepo : quickRepo || repositories[0]?.id || "";

  function memoInputFrom(memo: Memo, patch: Partial<SaveMemoInput> = {}): SaveMemoInput {
    return {
      id: memo.id,
      repository_id: patch.repository_id ?? memo.repository_id,
      title: patch.title ?? memo.title,
      body_md: patch.body_md ?? memo.body_md,
      tags: patch.tags ?? memo.tags,
      pinned: patch.pinned ?? memo.pinned,
      archived: patch.archived ?? memo.archived,
    };
  }

  function optimisticMemo(memo: Memo, patch: Partial<SaveMemoInput>): Memo {
    const body = patch.body_md ?? memo.body_md;
    return {
      ...memo,
      repository_id: patch.repository_id ?? memo.repository_id,
      title: patch.title ?? memo.title,
      body_md: body,
      tags: patch.tags ?? memo.tags,
      pinned: patch.pinned ?? memo.pinned,
      archived: patch.archived ?? memo.archived,
      updated_at: new Date().toISOString(),
      meta: { ...memo.meta, byte_len: body.length },
    };
  }

  function replaceMemo(saved: Memo) {
    setMemos((items) => [saved, ...items.filter((item) => item.id !== saved.id)]);
    setActiveMemoId(saved.id);
  }

  async function flushPendingSave() {
    if (saveTimerRef.current !== null) {
      window.clearTimeout(saveTimerRef.current);
      saveTimerRef.current = null;
    }
    const pending = pendingSaveRef.current;
    if (!pending) return;
    pendingSaveRef.current = null;
    setSaveText("Saving...");
    try {
      const saved = await saveMemo(pending);
      replaceMemo(saved);
      setSaveText("Saved");
    } catch (error) {
      pendingSaveRef.current = pending;
      setSaveText(error instanceof Error ? "Save failed" : "Save failed");
    }
  }

  function queueSave(patch: Partial<SaveMemoInput>) {
    if (!activeMemo) return;
    const optimistic = optimisticMemo(activeMemo, patch);
    setMemos((items) => [optimistic, ...items.filter((item) => item.id !== optimistic.id)]);
    setActiveMemoId(optimistic.id);
    pendingSaveRef.current = memoInputFrom(optimistic);
    setSaveText("Editing...");
    if (saveTimerRef.current !== null) window.clearTimeout(saveTimerRef.current);
    saveTimerRef.current = window.setTimeout(() => {
      void flushPendingSave();
    }, 650);
  }

  async function handleSave(patch: Partial<SaveMemoInput>, options: { debounce?: boolean } = {}) {
    const repo = activeMemo?.repository_id ?? (repositories[0]?.id || quickRepo);
    if (!repo) return;
    if (options.debounce && activeMemo) {
      queueSave(patch);
      return;
    }
    await flushPendingSave();
    const saved = await saveMemo(
      activeMemo
        ? memoInputFrom(activeMemo, patch)
        : {
            id: null,
            repository_id: repo,
            title: patch.title ?? "",
            body_md: patch.body_md ?? "",
            tags: patch.tags ?? [],
            pinned: patch.pinned ?? false,
            archived: patch.archived ?? false,
          },
    );
    replaceMemo(saved);
  }

  async function handleNewMemo() {
    const repo = activeRepo !== "all" ? activeRepo : repositories[0]?.id;
    if (!repo) return;
    const saved = await saveMemo({
      id: null,
      repository_id: repo,
      title: "Untitled memo",
      body_md: "",
      tags: [],
      pinned: false,
      archived: false,
    });
    setMemos((items) => [saved, ...items]);
    setActiveMemoId(saved.id);
    setMode("edit");
  }

  async function handleDelete(id: string) {
    await flushPendingSave();
    await deleteMemo(id);
    setMemos((items) => items.filter((item) => item.id !== id));
    setActiveMemoId(null);
  }

  async function handleClipboardCapture(repositoryId: string) {
    const saved = await captureClipboardMemo(repositoryId);
    setMemos((items) => [saved, ...items.filter((item) => item.id !== saved.id)]);
    setActiveMemoId(saved.id);
  }

  async function handleQuickSave() {
    if (!quickRepo || !quickText.trim()) return;
    const saved = await saveQuickMemo({
      id: null,
      repository_id: quickRepo,
      title: quickText.split("\n").find(Boolean)?.slice(0, 64) || "Quick memo",
      body_md: quickText,
      tags: ["quick"],
      pinned: false,
      archived: false,
    });
    setMemos((items) => [saved, ...items]);
    setActiveMemoId(saved.id);
    setQuickText("");
    setQuickOpen(false);
  }

  async function fillQuickFromClipboard() {
    setQuickText(await readClipboardText());
  }

  async function handleCreateRepo(name: string, temporary: boolean) {
    const repo = await createRepository(name, temporary, colors[repositories.length % colors.length]);
    setRepositories((items) => [...items, repo]);
    setQuickRepo(repo.id);
    setNewRepoOpen(false);
  }

  async function handleSync() {
    await flushPendingSave();
    setSyncText("Syncing");
    try {
      const result = await syncNow(serverUrl);
      setSyncText(`Pushed ${result.pushed}, pulled ${result.pulled}`);
      const refreshed = await bootstrap();
      applyBootstrap(refreshed, activeMemoId);
    } catch (error) {
      setSyncText(error instanceof Error ? error.message : String(error));
    }
  }

  async function handleSaveSettings(next: AppSettings) {
    const saved = await updateAppSettings(next);
    setSettings(saved);
    setServerUrl(saved.server_url);
    setMode(saved.writing_mode);
    setSidebarCollapsed(saved.compact_sidebar_on_start);
    setSyncText("Settings saved");
  }

  return (
    <main className={`shell ${isDesktopApp ? "desktop-shell" : "web-shell"} ${sidebarCollapsed ? "sidebar-collapsed" : ""}`}>
      {isDesktopApp && (
        <Titlebar
          onQuick={() => showQuickCaptureWindow()}
          onSettings={() => showSettingsWindow()}
          onToggleSidebar={() => setSidebarCollapsed((value) => !value)}
          sidebarCollapsed={sidebarCollapsed}
        />
      )}
      <section className="workspace">
        <aside className="sidebar">
          <div className="sidebar-top">
            <div>
              <p className="eyebrow">Repositories</p>
              <h1>Memo Sync</h1>
            </div>
            <div className="sidebar-actions">
              <button className="icon-button sidebar-toggle" title={sidebarCollapsed ? "Expand sidebar" : "Collapse sidebar"} onClick={() => setSidebarCollapsed((value) => !value)}>
                {sidebarCollapsed ? <PanelLeftOpen size={18} /> : <PanelLeftClose size={18} />}
              </button>
              <button className="icon-button" title="Create repository" onClick={() => setNewRepoOpen(true)}>
                <FolderPlus size={18} />
              </button>
            </div>
          </div>

          <button className={activeRepo === "all" ? "repo active" : "repo"} title="All notes" onClick={() => setActiveRepo("all")}>
            <span className="repo-dot all" />
            <span>All notes</span>
            <strong>{memos.filter((memo) => !memo.deleted).length}</strong>
          </button>

          {repositories.map((repo) => (
            <button
              key={repo.id}
              className={activeRepo === repo.id ? "repo active" : "repo"}
              title={`${repo.name} (${repo.kind === "Temporary" ? "temporary" : "sync"})`}
              onClick={() => setActiveRepo(repo.id)}
            >
              <span className="repo-dot" style={{ background: repo.color }} />
              <span>{repo.name}</span>
              <small>{repo.kind === "Temporary" ? "temp" : "sync"}</small>
            </button>
          ))}

          <div className="tag-panel">
            <div className="panel-label">
              <Tag size={15} />
              Tags
            </div>
            <div className="tags">
              {tags.map((tag) => (
                <button key={tag} className={tagFilter === tag ? "tag active" : "tag"} onClick={() => setTagFilter(tagFilter === tag ? null : tag)}>
                  {tag}
                </button>
              ))}
            </div>
          </div>

          <div className="sync-panel">
            <div className="panel-label">
              <Cloud size={15} />
              Sync
            </div>
            <input value={serverUrl} onChange={(event) => setServerUrl(event.target.value)} />
            <button className="primary" onClick={handleSync}>
              <Cloud size={16} />
              Sync now
            </button>
            <div className="sync-stats" aria-label="Local sync status">
              <span>
                <strong>{localStats.pending_operations}</strong>
                queued
              </span>
              <span>
                <strong>{localStats.memo_count}</strong>
                notes
              </span>
              <span>
                <strong>{localStats.last_server_sequence}</strong>
                seq
              </span>
            </div>
            <small>{syncText}</small>
          </div>

          {!isDesktopApp && (
            <div className="web-footer">
              <button onClick={() => setDialog("settings")}>
                <Settings size={15} />
                Settings
              </button>
              <button onClick={() => setDialog("about")}>
                <Info size={15} />
                About
              </button>
            </div>
          )}
        </aside>

        <section className="list-pane">
          <div className="searchbar">
            <Search size={18} />
            <input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Search text, tags, metadata" />
          </div>
          <div className="list-actions">
            <button className="primary" onClick={handleNewMemo}>
              <Plus size={17} />
              New memo
            </button>
            <button className="secondary" onClick={() => captureRepoId && handleClipboardCapture(captureRepoId)}>
              <Clipboard size={17} />
              Clipboard
            </button>
          </div>
          <div className="memo-list">
            {visibleMemos.map((memo) => (
              <button
                key={memo.id}
                className={activeMemo?.id === memo.id ? "memo-row active" : "memo-row"}
                onClick={() => {
                  void flushPendingSave();
                  setActiveMemoId(memo.id);
                }}
              >
                <div>
                  <strong>{memo.title}</strong>
                  <p>{memo.body_md.replace(/[#*_`]/g, "").slice(0, 110) || "Empty memo"}</p>
                </div>
                <span>{new Date(memo.updated_at).toLocaleDateString()}</span>
              </button>
            ))}
          </div>
        </section>

        <section className="editor-pane">
          {activeMemo ? (
            <>
              <div className="editor-header">
                <div>
                  <p className="eyebrow">{activeRepository?.name ?? "Repository"}</p>
                  <input className="title-input" value={activeMemo.title} onChange={(event) => handleSave({ title: event.target.value }, { debounce: true })} />
                </div>
                <div className="toolbar">
                  <button className={activeMemo.pinned ? "icon-button active" : "icon-button"} title="Pin" onClick={() => handleSave({ pinned: !activeMemo.pinned })}>
                    <Pin size={17} />
                  </button>
                  <button className="icon-button" title="Edit" onClick={() => setMode("edit")}>
                    <FileText size={17} />
                  </button>
                  <button className="icon-button" title="Preview" onClick={() => setMode("preview")}>
                    <Eye size={17} />
                  </button>
                  <button className="icon-button" title="Split" onClick={() => setMode("split")}>
                    <Code2 size={17} />
                  </button>
                  <button className={activeMemo.archived ? "icon-button active" : "icon-button"} title="Archive" onClick={() => handleSave({ archived: !activeMemo.archived })}>
                    <Archive size={17} />
                  </button>
                  <button className="icon-button danger" title="Delete" onClick={() => handleDelete(activeMemo.id)}>
                    <Trash2 size={17} />
                  </button>
                </div>
              </div>

              <div className="metadata-strip">
                <span>{activeMemo.meta.byte_len} bytes</span>
                <span>{activeMemo.source}</span>
                <span>{deviceId.slice(0, 24)}</span>
                <span className={saveText === "Saved" ? "save-state saved" : "save-state"}>{saveText}</span>
                <input
                  value={activeMemo.tags.join(", ")}
                  onChange={(event) =>
                    handleSave({
                      tags: tokenizeTags(event.target.value),
                    }, { debounce: true })
                  }
                  placeholder="tags"
                />
              </div>

              <div className={`editor-grid ${mode}`}>
                {mode !== "preview" && (
                  <textarea value={activeMemo.body_md} onChange={(event) => handleSave({ body_md: event.target.value }, { debounce: true })} spellCheck={false} />
                )}
                {mode !== "edit" && (
                  <article className="markdown">
                    <Suspense fallback={<p className="markdown-loading">Rendering preview...</p>}>
                      <MarkdownView>{activeMemo.body_md}</MarkdownView>
                    </Suspense>
                  </article>
                )}
              </div>
            </>
          ) : (
            <div className="empty-state">
              <Sparkles size={36} />
              <h2>No memo selected</h2>
              <button className="primary" onClick={handleNewMemo}>
                <Plus size={17} />
                Create one
              </button>
            </div>
          )}
        </section>
      </section>

      {quickOpen && (
        <div className="modal-backdrop">
          <div className="quick-modal">
            <div className="modal-head">
              <div>
                <p className="eyebrow">Quick capture</p>
                <h2>Record a memo</h2>
              </div>
              <button className="icon-button" title="Close dialog" aria-label="Close dialog" onClick={() => setQuickOpen(false)}>
                <X size={18} />
              </button>
            </div>
            <select value={quickRepo} onChange={(event) => setQuickRepo(event.target.value)}>
              {repositories.map((repo) => (
                <option value={repo.id} key={repo.id}>
                  {repo.name} {repo.kind === "Temporary" ? "(temporary)" : "(sync)"}
                </option>
              ))}
            </select>
            <textarea autoFocus value={quickText} onChange={(event) => setQuickText(event.target.value)} />
            <div className="modal-actions">
              <button className="secondary" onClick={fillQuickFromClipboard}>
                <Clipboard size={17} />
                From clipboard
              </button>
              <button className="primary" onClick={handleQuickSave}>
                <Check size={17} />
                Save
              </button>
            </div>
          </div>
        </div>
      )}

      {newRepoOpen && <RepositoryDialog onClose={() => setNewRepoOpen(false)} onCreate={handleCreateRepo} />}
      {dialog && (
        <AppDialog
          dialog={dialog}
          onClose={() => setDialog(null)}
          onDialog={setDialog}
          serverUrl={serverUrl}
          settings={settings}
          onSaveSettings={handleSaveSettings}
          deviceId={deviceId}
          localStats={localStats}
          isDesktop={isDesktopApp}
        />
      )}
    </main>
  );
}

function QuickCaptureWindow() {
  const [repositories, setRepositories] = useState<Repository[]>([]);
  const [quickRepo, setQuickRepo] = useState("");
  const [quickText, setQuickText] = useState("");
  const [message, setMessage] = useState("");
  const [captureMode, setCaptureMode] = useState<CaptureMode>("split");
  const textAreaRef = useRef<HTMLTextAreaElement>(null);
  const activeRef = useRef(false);
  const quickStats = useMemo(() => {
    if (!quickText) return "0 lines / 0 chars";
    const lines = quickText.split(/\r\n|\r|\n/).length;
    return `${lines} lines / ${quickText.length} chars`;
  }, [quickText]);

  useEffect(() => {
    bootstrap().then((data) => {
      setRepositories(data.repositories);
      setQuickRepo(data.repositories[0]?.id ?? "");
    });
    const unsubs: Array<() => void> = [];
    listenAppEvent(APP_EVENTS.openQuickCapture, () => {
      activeRef.current = true;
      setMessage("");
      window.setTimeout(() => textAreaRef.current?.focus(), 40);
    }).then((unsub) => unsubs.push(unsub));
    listenAppEvent(APP_EVENTS.clipboardCaptureRequested, async () => {
      setQuickText(await readClipboardText());
      activeRef.current = true;
      window.setTimeout(() => textAreaRef.current?.focus(), 40);
    }).then((unsub) => unsubs.push(unsub));
    listenCurrentWindowFocus((focused) => {
      if (!focused && activeRef.current) {
        activeRef.current = false;
        window.setTimeout(() => windowAction("window_close"), 120);
      }
    }).then((unsub) => unsubs.push(unsub));
    return () => unsubs.forEach((unsub) => unsub());
  }, []);

  async function save() {
    if (!quickRepo || !quickText.trim()) return;
    const saved = await saveQuickMemo({
      id: null,
      repository_id: quickRepo,
      title: quickText.split("\n").find(Boolean)?.slice(0, 64) || "Quick memo",
      body_md: quickText,
      tags: ["quick"],
      pinned: false,
      archived: false,
    });
    setMessage(`Saved: ${saved.title}`);
    await emitMemosChanged({ active_memo_id: saved.id });
    setQuickText("");
    activeRef.current = false;
    window.setTimeout(() => windowAction("window_close"), 180);
  }

  async function fillClipboard() {
    setQuickText(await readClipboardText());
    window.setTimeout(() => textAreaRef.current?.focus(), 20);
  }

  function clearQuickText() {
    setQuickText("");
    setMessage("");
    window.setTimeout(() => textAreaRef.current?.focus(), 20);
  }

  function insertQuickSnippet(before: string, after = "", placeholder = "") {
    const target = textAreaRef.current;
    if (!target) {
      setQuickText((text) => `${text}${before}${placeholder}${after}`);
      return;
    }
    const start = target.selectionStart;
    const end = target.selectionEnd;
    const selected = quickText.slice(start, end) || placeholder;
    const next = `${quickText.slice(0, start)}${before}${selected}${after}${quickText.slice(end)}`;
    setQuickText(next);
    window.setTimeout(() => {
      target.focus();
      target.setSelectionRange(start + before.length, start + before.length + selected.length);
    }, 0);
  }

  function handleQuickKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.nativeEvent.isComposing) return;
    if (event.key === "Tab") {
      event.preventDefault();
      const target = event.currentTarget;
      const start = target.selectionStart;
      const end = target.selectionEnd;
      const next = `${quickText.slice(0, start)}  ${quickText.slice(end)}`;
      setQuickText(next);
      window.setTimeout(() => target.setSelectionRange(start + 2, start + 2), 0);
      return;
    }
    if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
      event.preventDefault();
      void save();
    }
  }

  return (
    <main className="capture-page">
      <WindowChrome title="Quick Capture" subtitle="Memo Sync" compact />
      <section className="capture-surface">
        <div className="capture-head">
          <div>
            <p className="eyebrow">Repository</p>
            <select value={quickRepo} onChange={(event) => setQuickRepo(event.target.value)}>
              {repositories.map((repo) => (
                <option value={repo.id} key={repo.id}>
                  {repo.name} {repo.kind === "Temporary" ? "(temporary)" : "(sync)"}
                </option>
              ))}
            </select>
          </div>
          <small>{message}</small>
        </div>
        <div className="capture-tools">
          <div className="capture-format-tools">
            <button title="Heading" onClick={() => insertQuickSnippet("# ", "", "Heading")}>
              <Heading1 size={15} />
            </button>
            <button title="Code block" onClick={() => insertQuickSnippet("```rust\n", "\n```", "fn main() {\n  \n}")}>
              <Code2 size={15} />
            </button>
            <button title="Quote" onClick={() => insertQuickSnippet("> ", "", "Quote")}>
              <Quote size={15} />
            </button>
            <button title="List" onClick={() => insertQuickSnippet("- ", "", "Item")}>
              <List size={15} />
            </button>
          </div>
          <div className="capture-mode-tools">
            <button className={captureMode === "edit" ? "active" : ""} title="Editor" onClick={() => setCaptureMode("edit")}>
              <FileText size={15} />
            </button>
            <button className={captureMode === "split" ? "active" : ""} title="Split" onClick={() => setCaptureMode("split")}>
              <Code2 size={15} />
            </button>
            <button className={captureMode === "preview" ? "active" : ""} title="Preview" onClick={() => setCaptureMode("preview")}>
              <Eye size={15} />
            </button>
          </div>
        </div>
        <div className={`capture-compose ${captureMode}`}>
          <textarea
            ref={textAreaRef}
            autoFocus
            value={quickText}
            onChange={(event) => setQuickText(event.target.value)}
            onKeyDown={handleQuickKeyDown}
            spellCheck={false}
            placeholder="Paste an idea, code, or a line you want to keep. Ctrl+Enter saves."
          />
          <article className="capture-preview markdown">
            {quickText.trim() ? (
              <Suspense fallback={<p className="markdown-loading">Rendering preview...</p>}>
                <MarkdownView>{quickText}</MarkdownView>
              </Suspense>
            ) : (
              <p className="preview-empty">Markdown preview</p>
            )}
          </article>
        </div>
      </section>
      <div className="capture-actions">
        <small>
          <span>{quickStats}</span>
          <span>Ctrl+Enter saves, Enter adds a line, Tab indents.</span>
        </small>
        <button className="secondary danger-soft" onClick={clearQuickText} disabled={!quickText}>
          <Eraser size={17} />
          Clear
        </button>
        <button className="secondary" onClick={fillClipboard}>
          <Clipboard size={17} />
          From clipboard
        </button>
        <button className="primary" onClick={save}>
          <Check size={17} />
          Save
        </button>
      </div>
    </main>
  );
}

function SettingsWindow() {
  const [settings, setSettings] = useState<AppSettings>(defaultSettings);
  const [deviceId, setDeviceId] = useState("");
  const [localStats, setLocalStats] = useState<LocalStats>(emptyStats);
  const [dialog, setDialog] = useState<Exclude<Dialog, null>>("settings");

  useEffect(() => {
    bootstrap().then((data) => {
      setSettings(data.settings);
      setDeviceId(data.device_id);
      setLocalStats(data.local_stats);
    });
  }, []);

  async function saveSettings(next: AppSettings) {
    const saved = await updateAppSettings(next);
    setSettings(saved);
  }

  return (
    <main className="settings-page">
      <WindowChrome title="Memo Sync Settings" subtitle="Preferences" />
      <AppDialog
        dialog={dialog}
        onClose={() => windowAction("window_close")}
        onDialog={(next) => next && setDialog(next)}
        serverUrl={settings.server_url}
        settings={settings}
        onSaveSettings={saveSettings}
        deviceId={deviceId}
        localStats={localStats}
        isDesktop={true}
        standalone
      />
    </main>
  );
}

function WindowChrome({ title, subtitle, compact = false }: { title: string; subtitle: string; compact?: boolean }) {
  return (
    <header className={compact ? "window-chrome compact" : "window-chrome"} data-tauri-drag-region>
      <div className="window-chrome-title" data-tauri-drag-region>
        <span className="window-mark" data-tauri-drag-region>
          <MonitorCog size={15} />
        </span>
        <div data-tauri-drag-region>
          <strong data-tauri-drag-region>{title}</strong>
          <small data-tauri-drag-region>{subtitle}</small>
        </div>
      </div>
      <div className="window-chrome-actions">
        <button title="Minimize" aria-label="Minimize window" onClick={() => windowAction("window_minimize")}>
          <Minimize2 size={14} />
        </button>
        <button title="Close" aria-label="Close window" onClick={() => windowAction("window_close")}>
          <X size={15} />
        </button>
      </div>
    </header>
  );
}

function Titlebar({
  onQuick,
  onSettings,
  onToggleSidebar,
  sidebarCollapsed,
}: {
  onQuick: () => void;
  onSettings: () => void;
  onToggleSidebar: () => void;
  sidebarCollapsed: boolean;
}) {
  return (
    <header className="titlebar" data-tauri-drag-region>
      <div className="traffic">
        <button className="dot close" title="Close" aria-label="Close window" onClick={(event) => { event.stopPropagation(); windowAction("window_close"); }} />
        <button className="dot min" title="Minimize" aria-label="Minimize window" onClick={(event) => { event.stopPropagation(); windowAction("window_minimize"); }} />
        <button className="dot max" title="Maximize" aria-label="Maximize window" onClick={(event) => { event.stopPropagation(); windowAction("window_toggle_maximize"); }} />
      </div>
      <div className="titlebar-center" data-tauri-drag-region>
        <button title={sidebarCollapsed ? "Expand sidebar" : "Collapse sidebar"} onClick={(event) => { event.stopPropagation(); onToggleSidebar(); }}>
          <PanelLeft size={15} />
        </button>
        <span data-tauri-drag-region>Memo Sync</span>
      </div>
      <div className="titlebar-actions">
        <button title="Quick capture" onClick={(event) => { event.stopPropagation(); onQuick(); }}>
          <Sparkles size={15} />
        </button>
        <button title="Settings" onClick={(event) => { event.stopPropagation(); onSettings(); }}>
          <Settings size={15} />
        </button>
        <button title="Maximize" onClick={(event) => { event.stopPropagation(); windowAction("window_toggle_maximize"); }}>
          <Maximize2 size={15} />
        </button>
        <button title="Minimize" onClick={(event) => { event.stopPropagation(); windowAction("window_minimize"); }}>
          <Minimize2 size={15} />
        </button>
      </div>
    </header>
  );
}

function AppDialog({
  dialog,
  onClose,
  onDialog,
  serverUrl,
  settings,
  onSaveSettings,
  deviceId,
  localStats,
  isDesktop,
  standalone = false,
}: {
  dialog: Exclude<Dialog, null>;
  onClose: () => void;
  onDialog: (dialog: Dialog) => void;
  serverUrl: string;
  settings: AppSettings;
  onSaveSettings: (settings: AppSettings) => Promise<void>;
  deviceId: string;
  localStats: LocalStats;
  isDesktop: boolean;
  standalone?: boolean;
}) {
  const title = dialog === "settings" ? "Settings" : dialog === "shortcuts" ? "Shortcuts" : "About";
  const [draft, setDraft] = useState<AppSettings>({ ...settings, server_url: serverUrl });
  const [message, setMessage] = useState("");
  const [checking, setChecking] = useState(false);

  useEffect(() => {
    setDraft({ ...settings, server_url: serverUrl });
    setMessage("");
  }, [settings, serverUrl, dialog]);

  async function saveDraft() {
    setMessage("Saving...");
    try {
      await onSaveSettings(draft);
      setMessage("Saved");
    } catch (error) {
      setMessage(error instanceof Error ? error.message : String(error));
    }
  }

  async function checkDraftShortcuts() {
    setChecking(true);
    setMessage("Checking shortcuts...");
    try {
      const result = await checkShortcuts(
        draft.quick_capture_shortcut,
        draft.clipboard_capture_shortcut,
        draft.settings_shortcut,
      );
      setMessage(result.message);
    } catch (error) {
      setMessage(error instanceof Error ? error.message : String(error));
    } finally {
      setChecking(false);
    }
  }

  return (
    <div className={standalone ? "app-modal-shell" : "modal-backdrop"}>
      <div className={standalone ? "app-modal standalone" : "app-modal"}>
        {!standalone && (
          <div className="modal-head">
            <div>
              <p className="eyebrow">{dialog}</p>
              <h2>{title}</h2>
            </div>
            <button className="icon-button" title="Close dialog" aria-label="Close dialog" onClick={onClose}>
              <X size={18} />
            </button>
          </div>
        )}
        {standalone && (
          <div className="standalone-heading">
            <p className="eyebrow">{dialog}</p>
            <h2>{title}</h2>
          </div>
        )}
        <div className="modal-tabs">
          <button className={dialog === "settings" ? "active" : ""} onClick={() => onDialog("settings")}>
            <Settings size={15} />
            Settings
          </button>
          {isDesktop && (
            <button className={dialog === "shortcuts" ? "active" : ""} onClick={() => onDialog("shortcuts")}>
              <Keyboard size={15} />
              Shortcuts
            </button>
          )}
          <button className={dialog === "about" ? "active" : ""} onClick={() => onDialog("about")}>
            <Copyright size={15} />
            About
          </button>
        </div>
        {dialog === "settings" && (
          <div className="settings-grid">
            <label>
              <span>Sync endpoint</span>
              <input value={draft.server_url} onChange={(event) => setDraft({ ...draft, server_url: event.target.value })} />
            </label>
            <label>
              <span>Device</span>
              <input value={deviceId || "web-preview"} readOnly />
            </label>
            <label>
              <span>Writing mode</span>
              <select value={draft.writing_mode} onChange={(event) => setDraft({ ...draft, writing_mode: event.target.value as AppSettings["writing_mode"] })}>
                <option value="split">Editor and preview</option>
                <option value="edit">Editor first</option>
                <option value="preview">Preview first</option>
              </select>
            </label>
            <label className="toggle setting-toggle">
              <input
                type="checkbox"
                checked={draft.compact_sidebar_on_start}
                onChange={(event) => setDraft({ ...draft, compact_sidebar_on_start: event.target.checked })}
              />
              <span>Start with compact sidebar</span>
            </label>
            <label className="toggle setting-toggle">
              <input
                type="checkbox"
                checked={draft.auto_sync_enabled}
                onChange={(event) => setDraft({ ...draft, auto_sync_enabled: event.target.checked })}
              />
              <span>Background sync</span>
            </label>
            <label className="toggle setting-toggle">
              <input
                type="checkbox"
                checked={draft.realtime_sync_enabled}
                onChange={(event) => setDraft({ ...draft, realtime_sync_enabled: event.target.checked })}
                disabled={!draft.auto_sync_enabled}
              />
              <span>Realtime remote wakeup</span>
            </label>
            <label>
              <span>Background sync interval</span>
              <select
                value={draft.auto_sync_interval_secs}
                onChange={(event) => setDraft({ ...draft, auto_sync_interval_secs: Number(event.target.value) })}
                disabled={!draft.auto_sync_enabled}
              >
                <option value={15}>15 seconds</option>
                <option value={30}>30 seconds</option>
                <option value={60}>1 minute</option>
                <option value={300}>5 minutes</option>
                <option value={900}>15 minutes</option>
              </select>
            </label>
            <div className="settings-health">
              <div>
                <span>Queued operations</span>
                <strong>{localStats.pending_operations}</strong>
              </div>
              <div>
                <span>Known server sequence</span>
                <strong>{localStats.last_server_sequence}</strong>
              </div>
            </div>
            <div className="settings-actions">
              <button className="primary" onClick={saveDraft}>
                <Check size={16} />
                Save settings
              </button>
              <small>{message}</small>
            </div>
          </div>
        )}
        {dialog === "shortcuts" && (
          <div className="shortcut-list">
            <label>
              <span>Quick capture</span>
              <ShortcutRecorder
                value={draft.quick_capture_shortcut}
                onChange={(quick_capture_shortcut) => setDraft({ ...draft, quick_capture_shortcut })}
              />
            </label>
            <label>
              <span>Clipboard capture</span>
              <ShortcutRecorder
                value={draft.clipboard_capture_shortcut}
                onChange={(clipboard_capture_shortcut) => setDraft({ ...draft, clipboard_capture_shortcut })}
              />
            </label>
            <label>
              <span>Open settings</span>
              <ShortcutRecorder
                value={draft.settings_shortcut}
                onChange={(settings_shortcut) => setDraft({ ...draft, settings_shortcut })}
              />
            </label>
            <p>Click a field, press a shortcut, then check whether the OS accepts it. Examples: <code>Ctrl+Shift+KeyM</code>, <code>CmdOrCtrl+Space</code>, <code>Alt+KeyR</code>.</p>
            <div className="settings-actions">
              <button className="secondary" onClick={checkDraftShortcuts} disabled={checking}>
                <Search size={16} />
                Check conflicts
              </button>
              <button className="primary" onClick={saveDraft}>
                <Keyboard size={16} />
                Apply shortcuts
              </button>
              <small>{message}</small>
            </div>
          </div>
        )}
        {dialog === "about" && (
          <div className="about-panel">
            <strong>Memo Sync</strong>
            <p>Local-first notes with repositories, Markdown, tray capture, and a Rust sync server.</p>
            <dl>
              <div>
                <dt>Notes</dt>
                <dd>{localStats.memo_count}</dd>
              </div>
              <div>
                <dt>Repositories</dt>
                <dd>{localStats.repository_count}</dd>
              </div>
              <div>
                <dt>Pending sync</dt>
                <dd>{localStats.pending_operations}</dd>
              </div>
              <div>
                <dt>Server sequence</dt>
                <dd>{localStats.last_server_sequence}</dd>
              </div>
            </dl>
            <small>Copyright 2026 Memo Sync Contributors. MIT licensed.</small>
          </div>
        )}
      </div>
    </div>
  );
}

function ShortcutRecorder({ value, onChange }: { value: string; onChange: (value: string) => void }) {
  const [recording, setRecording] = useState(false);

  return (
    <div className={recording ? "shortcut-recorder recording" : "shortcut-recorder"}>
      <input
        value={value}
        onChange={(event) => onChange(event.target.value)}
        onFocus={() => setRecording(true)}
        onBlur={() => setRecording(false)}
        onKeyDown={(event) => {
          const shortcut = shortcutFromKeyboardEvent(event);
          if (!shortcut) return;
          event.preventDefault();
          event.stopPropagation();
          onChange(shortcut);
        }}
        placeholder="Press a shortcut"
      />
      <small>{recording ? "Press keys now" : "Click to record"}</small>
    </div>
  );
}

function shortcutFromKeyboardEvent(event: KeyboardEvent<HTMLInputElement>) {
  const key = normalizeShortcutKey(event);
  if (!key) return "";
  const parts: string[] = [];
  if (event.ctrlKey || event.metaKey) parts.push(navigator.platform.toLowerCase().includes("mac") ? "CmdOrCtrl" : "Ctrl");
  if (event.shiftKey) parts.push("Shift");
  if (event.altKey) parts.push("Alt");
  if (!parts.length) return "";
  parts.push(key);
  return parts.join("+");
}

function normalizeShortcutKey(event: KeyboardEvent<HTMLInputElement>) {
  if (["Control", "Shift", "Alt", "Meta"].includes(event.key)) return "";
  if (/^Key[A-Z]$/.test(event.code)) return event.code;
  if (/^Digit[0-9]$/.test(event.code)) return event.code;
  const named: Record<string, string> = {
    Space: "Space",
    Enter: "Enter",
    Escape: "Escape",
    Tab: "Tab",
    Backspace: "Backspace",
    Delete: "Delete",
    ArrowUp: "ArrowUp",
    ArrowDown: "ArrowDown",
    ArrowLeft: "ArrowLeft",
    ArrowRight: "ArrowRight",
  };
  if (named[event.code]) return named[event.code];
  if (/^F([1-9]|1[0-2])$/.test(event.code)) return event.code;
  return "";
}

function RepositoryDialog({ onClose, onCreate }: { onClose: () => void; onCreate: (name: string, temporary: boolean) => void }) {
  const [name, setName] = useState("");
  const [temporary, setTemporary] = useState(false);
  return (
    <div className="modal-backdrop">
      <div className="repo-modal">
        <div className="modal-head">
          <div>
            <p className="eyebrow">Repository</p>
            <h2>New library</h2>
          </div>
          <button className="icon-button" title="Close dialog" aria-label="Close dialog" onClick={onClose}>
            <X size={18} />
          </button>
        </div>
        <input autoFocus value={name} onChange={(event) => setName(event.target.value)} placeholder="Research, Inbox, Drafts" />
        <label className="toggle">
          <input type="checkbox" checked={temporary} onChange={(event) => setTemporary(event.target.checked)} />
          <span>Temporary, cleared outside sync</span>
        </label>
        <button className="primary" onClick={() => name.trim() && onCreate(name.trim(), temporary)}>
          <Check size={17} />
          Create
        </button>
      </div>
    </div>
  );
}
