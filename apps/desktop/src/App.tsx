import {
  Archive,
  Bold,
  Check,
  Clipboard,
  Cloud,
  Copyright,
  Code2,
  Copy,
  Eye,
  Eraser,
  FileText,
  FolderPlus,
  Heading1,
  Info,
  Italic,
  Keyboard,
  Link,
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
import { type CSSProperties, type KeyboardEvent, useCallback, useEffect, useMemo, useRef, useState } from "react";
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
  searchMemos,
  showQuickCaptureWindow,
  showSettingsWindow,
  syncNow,
  updateAppSettings,
  updateRepository,
  windowAction,
} from "./tauri";
import { memoSearchText, textStatsLabel, tokenizeTags } from "./search";
import { CommandPalette, type CommandItem } from "./components/CommandPalette";
import { MemoList } from "./components/MemoList";
import { ToastStack, type ToastKind, type ToastMessage } from "./components/ToastStack";
import { TypstPreview } from "./components/TypstPreview";

const colors = ["#c86f52", "#6f8f83", "#5f7597", "#9a7a42", "#8a6fa8"];
const defaultSettings: AppSettings = {
  server_url: "http://127.0.0.1:7373",
  quick_capture_shortcut: "Ctrl+Shift+KeyM",
  clipboard_capture_shortcut: "Ctrl+Shift+Alt+KeyV",
  settings_shortcut: "Ctrl+Shift+KeyS",
  writing_mode: "split",
  preview_render_path: "typst-inline",
  preview_template: "literary",
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
type ViewFilter = "active" | "pinned" | "archived" | "clipboard" | "quick";

const viewFilters: Array<{ id: ViewFilter; label: string; icon: typeof FileText }> = [
  { id: "active", label: "Inbox", icon: FileText },
  { id: "pinned", label: "Pinned", icon: Pin },
  { id: "archived", label: "Archive", icon: Archive },
  { id: "clipboard", label: "Clips", icon: Clipboard },
  { id: "quick", label: "Quick", icon: Sparkles },
];

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
  const [allTags, setAllTags] = useState<string[]>([]);
  const [activeRepo, setActiveRepo] = useState<string | "all">("all");
  const [activeMemoId, setActiveMemoId] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [tagFilter, setTagFilter] = useState<string | null>(null);
  const [viewFilter, setViewFilter] = useState<ViewFilter>("active");
  const [mode, setMode] = useState<Mode>("split");
  const [quickOpen, setQuickOpen] = useState(false);
  const [quickText, setQuickText] = useState("");
  const [quickRepo, setQuickRepo] = useState<string>("");
  const [newRepoOpen, setNewRepoOpen] = useState(false);
  const [editingRepo, setEditingRepo] = useState<Repository | null>(null);
  const [selectedMemoIds, setSelectedMemoIds] = useState<Set<string>>(new Set());
  const [serverUrl, setServerUrl] = useState("http://127.0.0.1:7373");
  const [settings, setSettings] = useState<AppSettings>(defaultSettings);
  const [syncText, setSyncText] = useState("Idle");
  const [localStats, setLocalStats] = useState<LocalStats>(emptyStats);
  const [deviceId, setDeviceId] = useState("");
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [dialog, setDialog] = useState<Dialog>(null);
  const [saveText, setSaveText] = useState("Saved");
  const [commandOpen, setCommandOpen] = useState(false);
  const [toasts, setToasts] = useState<ToastMessage[]>([]);
  const quickRepoRef = useRef("");
  const repositoriesRef = useRef<Repository[]>([]);
  const searchInputRef = useRef<HTMLInputElement>(null);
  const editorRef = useRef<HTMLTextAreaElement>(null);
  const searchRequestRef = useRef(0);
  const saveTimerRef = useRef<number | null>(null);
  const pendingSaveRef = useRef<SaveMemoInput | null>(null);
  const toastIdRef = useRef(0);

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
        if (!payload.background) notify("success", "Sync completed", `Pushed ${payload.pushed}, pulled ${payload.pulled}`);
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

  useEffect(() => {
    if (!isDesktopApp) return;
    const requestId = searchRequestRef.current + 1;
    searchRequestRef.current = requestId;
    const handle = window.setTimeout(() => {
      void searchMemos({
        repository_id: activeRepo === "all" ? null : activeRepo,
        query: query.trim() || null,
        tags: tagFilter ? [tagFilter] : [],
        pinned: viewFilter === "pinned" ? true : null,
        archived:
          viewFilter === "archived"
            ? true
            : viewFilter === "active" || viewFilter === "pinned" || viewFilter === "clipboard" || viewFilter === "quick"
              ? false
              : null,
        source: viewFilter === "clipboard" ? "Clipboard" : viewFilter === "quick" ? "QuickCapture" : null,
      }).then((results) => {
        if (searchRequestRef.current !== requestId) return;
        setMemos(results);
        setActiveMemoId((current) => (current && results.some((memo) => memo.id === current) ? current : results[0]?.id ?? null));
      }).catch((error) => {
        if (searchRequestRef.current === requestId) setSyncText(error instanceof Error ? error.message : String(error));
      });
    }, 140);
    return () => window.clearTimeout(handle);
  }, [activeRepo, query, tagFilter, viewFilter]);

  function applyBootstrap(data: Awaited<ReturnType<typeof bootstrap>>, preferredMemoId?: string | null) {
    setRepositories(data.repositories);
    setMemos(data.memos);
    setAllTags([...new Set(data.memos.flatMap((memo) => memo.tags))].sort());
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
      setViewFilter("active");
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
      if (viewFilter === "active" && memo.archived) return false;
      if (viewFilter === "pinned" && (!memo.pinned || memo.archived)) return false;
      if (viewFilter === "archived" && !memo.archived) return false;
      if (viewFilter === "clipboard" && (memo.source !== "Clipboard" || memo.archived)) return false;
      if (viewFilter === "quick" && (memo.source !== "QuickCapture" || memo.archived)) return false;
      if (!lower) return true;
      return memoSearchText(memo).includes(lower);
    });
  }, [activeRepo, memos, query, tagFilter, viewFilter]);

  const activeMemo = visibleMemos.find((memo) => memo.id === activeMemoId) ?? visibleMemos[0] ?? null;
  const tags = allTags;
  const activeRepository = repositories.find((repo) => repo.id === activeMemo?.repository_id);
  const activeRepoName = activeRepo === "all" ? "All notes" : repositories.find((repo) => repo.id === activeRepo)?.name ?? "Repository";
  const activeViewLabel = viewFilters.find((item) => item.id === viewFilter)?.label ?? "Inbox";
  const captureRepoId = activeRepo !== "all" ? activeRepo : quickRepo || repositories[0]?.id || "";
  const selectedMemos = useMemo(() => visibleMemos.filter((memo) => selectedMemoIds.has(memo.id)), [selectedMemoIds, visibleMemos]);
  const activeMemoStats = useMemo(() => textStatsLabel(activeMemo?.body_md ?? ""), [activeMemo?.body_md]);

  useEffect(() => {
    setSelectedMemoIds((current) => {
      if (!current.size) return current;
      const visibleIds = new Set(visibleMemos.map((memo) => memo.id));
      const next = new Set([...current].filter((id) => visibleIds.has(id)));
      return next.size === current.size ? current : next;
    });
  }, [visibleMemos]);

  const notify = useCallback((kind: ToastKind, title: string, detail?: string, action?: Pick<ToastMessage, "actionLabel" | "action">) => {
    const id = toastIdRef.current + 1;
    toastIdRef.current = id;
    setToasts((items) => [...items.slice(-3), { id, kind, title, detail, ...action }]);
    window.setTimeout(() => {
      setToasts((items) => items.filter((item) => item.id !== id));
    }, action?.action ? 8200 : kind === "error" ? 5200 : 3200);
  }, []);

  const selectMemoByOffset = useCallback(
    (offset: number) => {
      if (!visibleMemos.length) return;
      const currentIndex = Math.max(
        0,
        visibleMemos.findIndex((memo) => memo.id === (activeMemoId ?? activeMemo?.id)),
      );
      const nextIndex = Math.min(Math.max(currentIndex + offset, 0), visibleMemos.length - 1);
      setActiveMemoId(visibleMemos[nextIndex].id);
    },
    [activeMemo?.id, activeMemoId, visibleMemos],
  );

  useEffect(() => {
    const onKeyDown = (event: globalThis.KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      const tagName = target?.tagName;
      const isTyping = tagName === "INPUT" || tagName === "TEXTAREA" || tagName === "SELECT" || target?.isContentEditable;
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        searchInputRef.current?.focus();
        searchInputRef.current?.select();
        return;
      }
      if ((event.ctrlKey || event.metaKey) && event.shiftKey && event.key.toLowerCase() === "p") {
        event.preventDefault();
        setCommandOpen(true);
        return;
      }
      if (event.key === "Escape" && document.activeElement === searchInputRef.current) {
        event.preventDefault();
        setQuery("");
        searchInputRef.current?.blur();
        return;
      }
      if (isTyping) return;
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "n") {
        event.preventDefault();
        void handleNewMemo();
        return;
      }
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "j") {
        event.preventDefault();
        showQuickCaptureWindow();
        return;
      }
      if (event.key === "ArrowDown") {
        event.preventDefault();
        selectMemoByOffset(1);
      } else if (event.key === "ArrowUp") {
        event.preventDefault();
        selectMemoByOffset(-1);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [selectMemoByOffset]);

  const commands: CommandItem[] = [
    {
      id: "new-memo",
      title: "New memo",
      category: "Action",
      detail: activeRepo === "all" ? "Create in the first repository" : "Create in current repository",
      shortcut: "Ctrl N",
      run: () => handleNewMemo(),
    },
    {
      id: "quick-capture",
      title: "Quick capture",
      category: "Action",
      detail: "Open the floating capture window",
      shortcut: "Ctrl J",
      run: () => showQuickCaptureWindow(),
    },
    {
      id: "clipboard",
      title: "Capture clipboard",
      category: "Action",
      detail: "Save clipboard text into the selected repository",
      run: () => {
        if (captureRepoId) return handleClipboardCapture(captureRepoId);
      },
    },
    {
      id: "sync",
      title: "Sync now",
      category: "Action",
      detail: syncText,
      run: () => handleSync(),
    },
    {
      id: "search",
      title: "Focus search",
      category: "Action",
      detail: "Search text, tags, and metadata",
      shortcut: "Ctrl K",
      run: () => {
        searchInputRef.current?.focus();
        searchInputRef.current?.select();
      },
    },
    {
      id: "clear-filters",
      title: "Clear filters",
      category: "Action",
      detail: "Show all notes again",
      run: () => {
        setActiveRepo("all");
        setTagFilter(null);
        setQuery("");
        setViewFilter("active");
      },
    },
    ...(activeMemo
      ? [
          {
            id: "duplicate-current",
            title: "Duplicate current memo",
            category: "Action",
            detail: activeMemo.title,
            run: () => handleDuplicateMemo(activeMemo),
          },
          {
            id: "archive-current",
            title: activeMemo.archived ? "Restore current memo" : "Archive current memo",
            category: "Action",
            detail: activeMemo.title,
            run: () => handleArchiveToggle(activeMemo),
          },
          {
            id: "delete-current",
            title: "Delete current memo",
            category: "Action",
            detail: activeMemo.title,
            run: () => handleDelete(activeMemo.id),
          },
        ]
      : []),
    ...(activeMemo
      ? repositories
          .filter((repo) => repo.id !== activeMemo.repository_id)
          .map((repo) => ({
            id: `move-current-${repo.id}`,
            title: `Move to ${repo.name}`,
            category: "Move",
            detail: repo.kind === "Temporary" ? "Temporary repository" : "Persistent repository",
            run: () => handleMoveMemo(activeMemo, repo.id),
          }))
      : []),
    ...(selectedMemos.length
      ? [
          {
            id: "archive-selected",
            title: "Archive selected memos",
            category: "Batch",
            detail: `${selectedMemos.length} selected`,
            run: () => handleBatchArchive(),
          },
          {
            id: "delete-selected",
            title: "Delete selected memos",
            category: "Batch",
            detail: `${selectedMemos.length} selected`,
            run: () => handleBatchDelete(),
          },
          {
            id: "clear-selection",
            title: "Clear selection",
            category: "Batch",
            detail: `${selectedMemos.length} selected`,
            run: () => setSelectedMemoIds(new Set()),
          },
        ]
      : []),
    ...viewFilters.map((item) => ({
      id: `view-${item.id}`,
      title: item.label,
      category: "View",
      detail: "Switch memo list",
      run: () => setViewFilter(item.id),
    })),
    {
      id: "mode-split",
      title: "Editor and preview",
      category: "View",
      detail: "Use split writing mode",
      run: () => setMode("split"),
    },
    {
      id: "mode-edit",
      title: "Editor only",
      category: "View",
      detail: "Focus on Markdown input",
      run: () => setMode("edit"),
    },
    {
      id: "mode-preview",
      title: "Preview only",
      category: "View",
      detail: "Read the rendered memo",
      run: () => setMode("preview"),
    },
    {
      id: "toggle-sidebar",
      title: sidebarCollapsed ? "Expand sidebar" : "Collapse sidebar",
      category: "View",
      detail: "Change navigation density",
      run: () => setSidebarCollapsed((value) => !value),
    },
    {
      id: "settings",
      title: "Settings",
      category: "App",
      detail: "Sync endpoint, shortcuts, and about",
      run: () => (isDesktopApp ? showSettingsWindow() : setDialog("settings")),
    },
    ...(activeRepo !== "all" && activeRepository
      ? [
          {
            id: "manage-repository",
            title: "Manage repository",
            category: "Repository",
            detail: activeRepository.name,
            run: () => setEditingRepo(activeRepository),
          },
        ]
      : []),
    ...repositories.map((repo) => ({
      id: `repo-${repo.id}`,
      title: repo.name,
      category: "Repository",
      detail: repo.kind === "Temporary" ? "Temporary notes" : "Persistent sync repository",
      run: () => {
        setActiveRepo(repo.id);
        setTagFilter(null);
      },
    })),
    ...tags.slice(0, 32).map((tag) => ({
      id: `tag-${tag}`,
      title: tag,
      category: "Tag",
      detail: "Filter notes by tag",
      run: () => setTagFilter(tag),
    })),
    ...visibleMemos.slice(0, 24).map((memo) => ({
      id: `memo-${memo.id}`,
      title: memo.title,
      category: "Memo",
      detail: memo.body_md.replace(/[#*_`]/g, "").slice(0, 86) || "Empty memo",
      run: () => {
        setActiveRepo("all");
        setTagFilter(null);
        setQuery("");
        setActiveMemoId(memo.id);
      },
    })),
  ];

  const hasFilters = activeRepo !== "all" || viewFilter !== "active" || Boolean(tagFilter) || Boolean(query.trim());

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
    setAllTags((items) => [...new Set([...items, ...saved.tags])].sort());
    setActiveMemoId(saved.id);
  }

  function toggleMemoSelected(id: string) {
    setSelectedMemoIds((current) => {
      const next = new Set(current);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
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
      notify("error", "Save failed", error instanceof Error ? error.message : String(error));
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

  function updateEditorBody(nextBody: string, selection?: { start: number; end: number }) {
    if (!activeMemo) return;
    void handleSave({ body_md: nextBody }, { debounce: true });
    window.requestAnimationFrame(() => {
      editorRef.current?.focus();
      if (selection) editorRef.current?.setSelectionRange(selection.start, selection.end);
    });
  }

  function wrapEditorSelection(before: string, after = before, placeholder = "text") {
    if (!activeMemo) return;
    const element = editorRef.current;
    const body = activeMemo.body_md;
    const start = element?.selectionStart ?? body.length;
    const end = element?.selectionEnd ?? body.length;
    const selected = body.slice(start, end) || placeholder;
    const next = `${body.slice(0, start)}${before}${selected}${after}${body.slice(end)}`;
    updateEditorBody(next, { start: start + before.length, end: start + before.length + selected.length });
  }

  function prefixEditorLines(prefix: string, placeholder = "List item") {
    if (!activeMemo) return;
    const element = editorRef.current;
    const body = activeMemo.body_md;
    const start = element?.selectionStart ?? body.length;
    const end = element?.selectionEnd ?? body.length;
    const selected = body.slice(start, end) || placeholder;
    const prefixed = selected
      .split(/\r\n|\r|\n/)
      .map((line) => `${prefix}${line}`)
      .join("\n");
    const next = `${body.slice(0, start)}${prefixed}${body.slice(end)}`;
    updateEditorBody(next, { start, end: start + prefixed.length });
  }

  function insertEditorCodeBlock() {
    if (!activeMemo) return;
    const element = editorRef.current;
    const body = activeMemo.body_md;
    const start = element?.selectionStart ?? body.length;
    const end = element?.selectionEnd ?? body.length;
    const selected = body.slice(start, end) || "fn main() {\n  \n}";
    const block = `\n\`\`\`rust\n${selected}\n\`\`\`\n`;
    const next = `${body.slice(0, start)}${block}${body.slice(end)}`;
    const caret = start + block.length - 5;
    updateEditorBody(next, { start: caret, end: caret });
  }

  function handleEditorKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (!activeMemo) return;
    if (event.key === "Tab") {
      event.preventDefault();
      const element = event.currentTarget;
      const start = element.selectionStart;
      const end = element.selectionEnd;
      const next = `${activeMemo.body_md.slice(0, start)}  ${activeMemo.body_md.slice(end)}`;
      updateEditorBody(next, { start: start + 2, end: start + 2 });
      return;
    }
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "b") {
      event.preventDefault();
      wrapEditorSelection("**", "**", "bold");
      return;
    }
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "i") {
      event.preventDefault();
      wrapEditorSelection("*", "*", "italic");
      return;
    }
    if ((event.ctrlKey || event.metaKey) && event.key === "Enter") {
      event.preventDefault();
      void flushPendingSave();
    }
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
    setAllTags((items) => [...new Set([...items, ...saved.tags])].sort());
    setActiveMemoId(saved.id);
    setMode("edit");
    notify("success", "Memo created", activeRepo === "all" ? undefined : activeRepository?.name);
  }

  async function handleDuplicateMemo(memo: Memo) {
    await flushPendingSave();
    const saved = await saveMemo({
      id: null,
      repository_id: memo.repository_id,
      title: `${memo.title || "Untitled memo"} copy`,
      body_md: memo.body_md,
      tags: memo.tags,
      pinned: false,
      archived: false,
    });
    replaceMemo(saved);
    notify("success", "Memo duplicated", saved.title, {
      actionLabel: "Undo",
      action: async () => {
        await deleteMemo(saved.id);
        setMemos((items) => items.filter((item) => item.id !== saved.id));
        setActiveMemoId(memo.id);
      },
    });
  }

  async function handleMoveMemo(memo: Memo, repositoryId: string) {
    if (memo.repository_id === repositoryId) return;
    await flushPendingSave();
    const target = repositories.find((repo) => repo.id === repositoryId);
    const saved = await saveMemo(memoInputFrom(memo, { repository_id: repositoryId }));
    replaceMemo(saved);
    setActiveRepo(repositoryId);
    notify("info", "Memo moved", target?.name ?? "Repository", {
      actionLabel: "Undo",
      action: async () => {
        const restored = await saveMemo(memoInputFrom(saved, { repository_id: memo.repository_id }));
        replaceMemo(restored);
        setActiveRepo(memo.repository_id);
      },
    });
  }

  async function handleBatchArchive() {
    if (!selectedMemos.length) return;
    await flushPendingSave();
    const originals = selectedMemos;
    const saved: Memo[] = [];
    for (const memo of originals) {
      saved.push(await saveMemo(memoInputFrom(memo, { archived: true })));
    }
    setMemos((items) => [...saved, ...items.filter((item) => !saved.some((memo) => memo.id === item.id))]);
    setSelectedMemoIds(new Set());
    setActiveMemoId((current) => (current && saved.some((memo) => memo.id === current) ? visibleMemos.find((memo) => !selectedMemoIds.has(memo.id))?.id ?? null : current));
    notify("info", "Memos archived", `${saved.length} selected`, {
      actionLabel: "Undo",
      action: async () => {
        const restored: Memo[] = [];
        for (const memo of originals) {
          restored.push(await saveMemo(memoInputFrom(memo)));
        }
        setMemos((items) => [...restored, ...items.filter((item) => !restored.some((memo) => memo.id === item.id))]);
        setSelectedMemoIds(new Set(originals.map((memo) => memo.id)));
      },
    });
  }

  async function handleBatchMove(repositoryId: string) {
    if (!selectedMemos.length) return;
    await flushPendingSave();
    const originals = selectedMemos;
    const target = repositories.find((repo) => repo.id === repositoryId);
    const saved: Memo[] = [];
    for (const memo of originals) {
      saved.push(await saveMemo(memoInputFrom(memo, { repository_id: repositoryId })));
    }
    setMemos((items) => [...saved, ...items.filter((item) => !saved.some((memo) => memo.id === item.id))]);
    setSelectedMemoIds(new Set());
    setActiveRepo(repositoryId);
    setActiveMemoId(saved[0]?.id ?? null);
    notify("info", "Memos moved", `${saved.length} to ${target?.name ?? "repository"}`, {
      actionLabel: "Undo",
      action: async () => {
        const restored: Memo[] = [];
        for (const memo of originals) {
          restored.push(await saveMemo(memoInputFrom(memo)));
        }
        setMemos((items) => [...restored, ...items.filter((item) => !restored.some((memo) => memo.id === item.id))]);
        setSelectedMemoIds(new Set(originals.map((memo) => memo.id)));
      },
    });
  }

  async function handleBatchDelete() {
    if (!selectedMemos.length) return;
    await flushPendingSave();
    const originals = selectedMemos;
    for (const memo of originals) {
      await deleteMemo(memo.id);
    }
    setMemos((items) => items.filter((item) => !selectedMemoIds.has(item.id)));
    setSelectedMemoIds(new Set());
    setActiveMemoId((current) => (current && originals.some((memo) => memo.id === current) ? visibleMemos.find((memo) => !selectedMemoIds.has(memo.id))?.id ?? null : current));
    notify("warning", "Memos deleted", `${originals.length} selected`, {
      actionLabel: "Undo",
      action: async () => {
        const restored: Memo[] = [];
        for (const memo of originals) {
          restored.push(await saveMemo(memoInputFrom(memo)));
        }
        setMemos((items) => [...restored, ...items.filter((item) => !restored.some((memo) => memo.id === item.id))]);
        setSelectedMemoIds(new Set(originals.map((memo) => memo.id)));
        setActiveMemoId(restored[0]?.id ?? null);
      },
    });
  }

  async function handleDelete(id: string) {
    await flushPendingSave();
    const deleted = memos.find((memo) => memo.id === id);
    await deleteMemo(id);
    setMemos((items) => items.filter((item) => item.id !== id));
    setActiveMemoId(null);
    notify(
      "warning",
      "Memo deleted",
      deleted?.title,
      deleted
        ? {
            actionLabel: "Undo",
            action: async () => {
              const restored = await saveMemo(memoInputFrom(deleted));
              replaceMemo(restored);
              notify("success", "Memo restored", restored.title);
            },
          }
        : undefined,
    );
  }

  async function handleArchiveToggle(memo: Memo) {
    const archived = !memo.archived;
    const saved = await saveMemo(memoInputFrom(memo, { archived }));
    replaceMemo(saved);
    notify(archived ? "info" : "success", archived ? "Memo archived" : "Memo restored", memo.title, {
      actionLabel: "Undo",
      action: async () => {
        const restored = await saveMemo(memoInputFrom(memo));
        replaceMemo(restored);
        notify("success", "Archive undone", restored.title);
      },
    });
  }

  async function handleClipboardCapture(repositoryId: string) {
    const saved = await captureClipboardMemo(repositoryId);
    setMemos((items) => [saved, ...items.filter((item) => item.id !== saved.id)]);
    setAllTags((items) => [...new Set([...items, ...saved.tags])].sort());
    setActiveMemoId(saved.id);
    notify("success", "Clipboard captured", saved.title);
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
    setAllTags((items) => [...new Set([...items, ...saved.tags])].sort());
    setActiveMemoId(saved.id);
    setQuickText("");
    setQuickOpen(false);
    notify("success", "Quick memo saved", saved.title);
  }

  async function fillQuickFromClipboard() {
    setQuickText(await readClipboardText());
  }

  async function handleCreateRepo(name: string, temporary: boolean, color: string) {
    const repo = await createRepository(name, temporary, color);
    setRepositories((items) => [...items, repo]);
    setActiveRepo(repo.id);
    setQuickRepo(repo.id);
    setNewRepoOpen(false);
    setLocalStats((stats) => ({ ...stats, repository_count: stats.repository_count + 1 }));
    notify("success", "Repository created", repo.name);
  }

  async function handleUpdateRepo(repo: Repository, name: string, color: string, syncEnabled: boolean) {
    const saved = await updateRepository(repo.id, name, color, syncEnabled);
    setRepositories((items) => items.map((item) => (item.id === saved.id ? saved : item)));
    setEditingRepo(null);
    notify("success", "Repository updated", saved.name);
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
      notify("error", "Sync failed", error instanceof Error ? error.message : String(error));
    }
  }

  async function handleSaveSettings(next: AppSettings) {
    const saved = await updateAppSettings(next);
    setSettings(saved);
    setServerUrl(saved.server_url);
    setMode(saved.writing_mode);
    setSidebarCollapsed(saved.compact_sidebar_on_start);
    setSyncText("Settings saved");
    notify("success", "Settings saved");
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
            <strong>{localStats.memo_count}</strong>
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
            <input ref={searchInputRef} value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Search text, tags, metadata" />
          </div>
          <div className="list-context">
            <span>
              <strong>{visibleMemos.length}</strong>
              {visibleMemos.length === 1 ? "memo" : "memos"}
            </span>
            <button className={viewFilter !== "active" ? "context-chip active" : "context-chip"} onClick={() => setViewFilter("active")}>
              {activeViewLabel}
            </button>
            <button className={activeRepo !== "all" ? "context-chip active" : "context-chip"} onClick={() => setActiveRepo("all")}>
              {activeRepoName}
            </button>
            {tagFilter && (
              <button className="context-chip active" onClick={() => setTagFilter(null)}>
                #{tagFilter}
              </button>
            )}
            {query.trim() && (
              <button className="context-chip active" onClick={() => setQuery("")}>
                {query.trim()}
              </button>
            )}
            {hasFilters && (
              <button
                className="context-clear"
                onClick={() => {
                  setActiveRepo("all");
                  setTagFilter(null);
                  setQuery("");
                  setViewFilter("active");
                }}
              >
                Clear
              </button>
            )}
            {activeRepo !== "all" && activeRepository && (
              <button className="context-clear context-manage" onClick={() => setEditingRepo(activeRepository)}>
                Manage
              </button>
            )}
          </div>
          <div className="view-switcher" role="tablist" aria-label="Memo views">
            {viewFilters.map((item) => {
              const Icon = item.icon;
              return (
                <button key={item.id} className={viewFilter === item.id ? "active" : ""} onClick={() => setViewFilter(item.id)} aria-pressed={viewFilter === item.id}>
                  <Icon size={14} />
                  <span>{item.label}</span>
                </button>
              );
            })}
          </div>
          {selectedMemos.length > 0 && (
            <div className="batch-bar">
              <strong>{selectedMemos.length} selected</strong>
              <button onClick={handleBatchArchive}>
                <Archive size={14} />
                Archive
              </button>
              <select value="" onChange={(event) => event.target.value && handleBatchMove(event.target.value)}>
                <option value="">Move to...</option>
                {repositories.map((repo) => (
                  <option key={repo.id} value={repo.id}>
                    {repo.name}
                  </option>
                ))}
              </select>
              <button className="danger-soft" onClick={handleBatchDelete}>
                <Trash2 size={14} />
                Delete
              </button>
              <button className="icon-button" title="Clear selection" onClick={() => setSelectedMemoIds(new Set())}>
                <X size={14} />
              </button>
            </div>
          )}
          <div className="list-actions">
            <button className="primary" onClick={handleNewMemo}>
              <Plus size={17} />
              New memo
            </button>
            <button
              className="secondary"
              onClick={() =>
                setSelectedMemoIds((current) =>
                  current.size === visibleMemos.length ? new Set() : new Set(visibleMemos.map((memo) => memo.id)),
                )
              }
              disabled={!visibleMemos.length}
            >
              <Check size={17} />
              {selectedMemos.length === visibleMemos.length && visibleMemos.length ? "Clear" : "Select"}
            </button>
            <button className="secondary" onClick={() => captureRepoId && handleClipboardCapture(captureRepoId)}>
              <Clipboard size={17} />
              Clipboard
            </button>
          </div>
          <MemoList
            memos={visibleMemos}
            activeMemoId={activeMemo?.id ?? null}
            selectedIds={selectedMemoIds}
            onSelect={(id) => {
              void flushPendingSave();
              setActiveMemoId(id);
            }}
            onToggleSelected={toggleMemoSelected}
          />
        </section>

        <section className={`editor-pane editor-${mode}`}>
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
                  <button className="icon-button" title="Duplicate" onClick={() => handleDuplicateMemo(activeMemo)}>
                    <Copy size={17} />
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
                  <button className={activeMemo.archived ? "icon-button active" : "icon-button"} title="Archive" onClick={() => handleArchiveToggle(activeMemo)}>
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
                <span>{activeMemoStats}</span>
                <select className="repo-select" value={activeMemo.repository_id} onChange={(event) => handleMoveMemo(activeMemo, event.target.value)}>
                  {repositories.map((repo) => (
                    <option key={repo.id} value={repo.id}>
                      {repo.name}
                      {repo.kind === "Temporary" ? " (temp)" : ""}
                    </option>
                  ))}
                </select>
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

              {mode !== "preview" && (
                <div className="writebar" aria-label="Markdown tools">
                  <button title="Heading" onClick={() => prefixEditorLines("# ", "Heading")}>
                    <Heading1 size={15} />
                  </button>
                  <button title="Bold" onClick={() => wrapEditorSelection("**", "**", "bold")}>
                    <Bold size={15} />
                  </button>
                  <button title="Italic" onClick={() => wrapEditorSelection("*", "*", "italic")}>
                    <Italic size={15} />
                  </button>
                  <button title="List" onClick={() => prefixEditorLines("- ", "List item")}>
                    <List size={15} />
                  </button>
                  <button title="Quote" onClick={() => prefixEditorLines("> ", "Quote")}>
                    <Quote size={15} />
                  </button>
                  <button title="Code block" onClick={insertEditorCodeBlock}>
                    <Code2 size={15} />
                  </button>
                  <button title="Link" onClick={() => wrapEditorSelection("[", "](https://)", "link")}>
                    <Link size={15} />
                  </button>
                  <span>Ctrl+Enter saves now</span>
                </div>
              )}

              <div className={`editor-grid ${mode}`}>
                {mode !== "preview" && (
                  <textarea
                    ref={editorRef}
                    value={activeMemo.body_md}
                    onChange={(event) => handleSave({ body_md: event.target.value }, { debounce: true })}
                    onKeyDown={handleEditorKeyDown}
                    spellCheck={false}
                  />
                )}
                {mode !== "edit" && (
                  <article className="markdown preview-surface">
                    <TypstPreview
                      body={activeMemo.body_md}
                      format={activeMemo.tags.includes("typst") ? "typst" : "markdown"}
                      renderPath={settings.preview_render_path}
                      template={settings.preview_template}
                    />
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

      {newRepoOpen && <RepositoryDialog onClose={() => setNewRepoOpen(false)} onCreate={handleCreateRepo} colorIndex={repositories.length} />}
      {editingRepo && <RepositoryDialog repository={editingRepo} onClose={() => setEditingRepo(null)} onUpdate={handleUpdateRepo} />}
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
      <CommandPalette open={commandOpen} commands={commands} onClose={() => setCommandOpen(false)} />
      <ToastStack toasts={toasts} onDismiss={(id) => setToasts((items) => items.filter((item) => item.id !== id))} />
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
          <article className="capture-preview markdown preview-surface">
            {quickText.trim() ? (
              <TypstPreview body={quickText} format="markdown" renderPath="typst-inline" template="literary" />
            ) : (
              <p className="preview-empty">Typst preview</p>
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
            <label>
              <span>Preview render path</span>
              <select value={draft.preview_render_path} onChange={(event) => setDraft({ ...draft, preview_render_path: event.target.value as AppSettings["preview_render_path"] })}>
                <option value="typst-inline">Typst inline SVG</option>
                <option value="typst-asset">Typst asset protocol</option>
                <option value="markdown">React Markdown</option>
                <option value="auto">Auto</option>
              </select>
            </label>
            <label>
              <span>Typst template</span>
              <select value={draft.preview_template} onChange={(event) => setDraft({ ...draft, preview_template: event.target.value as AppSettings["preview_template"] })}>
                <option value="literary">Literary serif</option>
                <option value="compact">Compact notes</option>
                <option value="technical">Technical code</option>
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

function RepositoryDialog({
  repository,
  colorIndex = 0,
  onClose,
  onCreate,
  onUpdate,
}: {
  repository?: Repository;
  colorIndex?: number;
  onClose: () => void;
  onCreate?: (name: string, temporary: boolean, color: string) => void;
  onUpdate?: (repository: Repository, name: string, color: string, syncEnabled: boolean) => void;
}) {
  const [name, setName] = useState(repository?.name ?? "");
  const [temporary, setTemporary] = useState(repository?.kind === "Temporary");
  const [color, setColor] = useState(repository?.color ?? colors[colorIndex % colors.length]);
  const [syncEnabled, setSyncEnabled] = useState(repository?.sync_enabled ?? !temporary);
  const editing = Boolean(repository);
  const canSync = !temporary;
  const canSubmit = Boolean(name.trim());

  function submit() {
    if (!canSubmit) return;
    if (repository && onUpdate) {
      onUpdate(repository, name.trim(), color, canSync && syncEnabled);
      return;
    }
    onCreate?.(name.trim(), temporary, color);
  }

  return (
    <div className="modal-backdrop">
      <div className="repo-modal">
        <div className="modal-head">
          <div>
            <p className="eyebrow">Repository</p>
            <h2>{editing ? "Library details" : "New library"}</h2>
          </div>
          <button className="icon-button" title="Close dialog" aria-label="Close dialog" onClick={onClose}>
            <X size={18} />
          </button>
        </div>
        <input autoFocus value={name} onChange={(event) => setName(event.target.value)} onKeyDown={(event) => event.key === "Enter" && submit()} placeholder="Research, Inbox, Drafts" />
        <div className="color-swatches" aria-label="Repository color">
          {colors.map((item) => (
            <button
              key={item}
              className={color === item ? "active" : ""}
              style={{ "--swatch": item } as CSSProperties}
              title={item}
              onClick={() => setColor(item)}
            />
          ))}
        </div>
        <label className={editing ? "toggle disabled" : "toggle"}>
          <input
            type="checkbox"
            checked={temporary}
            disabled={editing}
            onChange={(event) => {
              setTemporary(event.target.checked);
              if (event.target.checked) setSyncEnabled(false);
            }}
          />
          <span>Temporary, cleared outside sync</span>
        </label>
        <label className={canSync ? "toggle" : "toggle disabled"}>
          <input type="checkbox" checked={canSync && syncEnabled} disabled={!canSync} onChange={(event) => setSyncEnabled(event.target.checked)} />
          <span>Sync this library</span>
        </label>
        <button className="primary" onClick={submit} disabled={!canSubmit}>
          <Check size={17} />
          {editing ? "Save library" : "Create"}
        </button>
      </div>
    </div>
  );
}
