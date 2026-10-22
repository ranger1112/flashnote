import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";
import { disable, enable, isEnabled } from "@tauri-apps/plugin-autostart";
import DOMPurify from "dompurify";
import { marked } from "marked";
import { accelFromEvent, formatAccel, matchesTerm, type NoteEntry } from "./lib";

interface Settings {
  notesDir: string;
  hotkey: string;
  pinned: boolean;
  captureSource: boolean;
  markdownMode: boolean;
  historyPreview: boolean;
  onboarded: boolean;
  window: unknown;
}

interface SavedNote {
  path: string;
  time: string;
}

interface ShowPayload {
  source: string | null;
}

interface StartupNotice {
  message: string;
  openSettings: boolean;
}

const DRAFT_KEY = "flashnote.draft";

function el<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) throw new Error(`missing element #${id}`);
  return node as T;
}

const textarea = el<HTMLTextAreaElement>("note");
const pinButton = el<HTMLButtonElement>("pin");
const settingsButton = el<HTMLButtonElement>("settings-btn");
const closeButton = el<HTMLButtonElement>("close-btn");
const notesButton = el<HTMLButtonElement>("notes-btn");
const historyPanel = el<HTMLElement>("history");
const historyClose = el<HTMLButtonElement>("history-close");
const historySearch = el<HTMLInputElement>("history-search");
const historyCount = el<HTMLSpanElement>("history-count");
const historyFolder = el<HTMLButtonElement>("history-folder");
const historyPreview = el<HTMLButtonElement>("history-preview");
const entryList = el<HTMLDivElement>("entry-list");
const settingsPanel = el<HTMLElement>("settings");
const settingsClose = el<HTMLButtonElement>("settings-close");
const hotkeyInput = el<HTMLInputElement>("hotkey-input");
const hotkeyRecord = el<HTMLButtonElement>("hotkey-record");
const hotkeyError = el<HTMLParagraphElement>("hotkey-error");
const notesDirInput = el<HTMLInputElement>("notes-dir");
const chooseDirButton = el<HTMLButtonElement>("choose-dir");
const captureSourceCheck = el<HTMLInputElement>("capture-source");
const markdownModeCheck = el<HTMLInputElement>("markdown-mode");
const autostartCheck = el<HTMLInputElement>("autostart");
const sourceLabel = el<HTMLSpanElement>("source");
const toastEl = el<HTMLDivElement>("toast");
const resizeGrip = el<HTMLDivElement>("resize-grip");
const welcomePanel = el<HTMLElement>("welcome");
const welcomeHotkey = el<HTMLSpanElement>("welcome-hotkey");
const welcomeDir = el<HTMLElement>("welcome-dir");
const welcomeStart = el<HTMLButtonElement>("welcome-start");
const welcomeSettings = el<HTMLButtonElement>("welcome-settings");

let settings: Settings = {
  notesDir: "",
  hotkey: "ctrl+alt+n",
  pinned: false,
  captureSource: true,
  markdownMode: false,
  historyPreview: false,
  onboarded: true,
  window: null,
};
let pinned = false;
let recording = false;
let toastTimer: number | undefined;
let draftTimer: number | undefined;
let searchTimer: number | undefined;
let appReady = false;
let notes: NoteEntry[] = [];
let noteOffset = 0;
let hasMoreNotes = false;
let serverFiltered = false;
let searchSeq = 0;

const PAGE_SIZE = 100;
const SEARCH_LIMIT = 200;

function toast(message: string, error = false, duration = 1600): void {
  toastEl.setAttribute("role", error ? "alert" : "status");
  toastEl.setAttribute("aria-live", error ? "assertive" : "polite");
  toastEl.textContent = message;
  toastEl.classList.toggle("error", error);
  toastEl.classList.add("show");
  if (toastTimer !== undefined) window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => toastEl.classList.remove("show"), duration);
}

function scheduleDraftSave(): void {
  if (draftTimer !== undefined) window.clearTimeout(draftTimer);
  draftTimer = window.setTimeout(() => {
    draftTimer = undefined;
    void persistDraft();
  }, 500);
}

async function persistDraft(): Promise<void> {
  try {
    await invoke("save_draft", { content: textarea.value });
  } catch {
    // 草稿写入失败不打断输入，下一次输入会重试
  }
}

async function flushDraft(): Promise<void> {
  if (draftTimer !== undefined) {
    window.clearTimeout(draftTimer);
    draftTimer = undefined;
  }
  await persistDraft();
}

/** 面板打开时让背景不可聚焦，关闭后恢复。 */
function setBackgroundInert(inert: boolean): void {
  for (const selector of ["header", "main", "footer", "#toast", "#resize-grip"]) {
    const node = document.querySelector<HTMLElement>(selector);
    if (node) node.inert = inert;
  }
}

/** 把 Tab 焦点限制在当前打开的面板内。 */
function trapFocus(event: KeyboardEvent): void {
  if (event.key !== "Tab") return;
  const panel = [welcomePanel, settingsPanel, historyPanel].find(
    (node) => !node.classList.contains("hidden"),
  );
  if (!panel) return;

  const focusables = Array.from(
    panel.querySelectorAll<HTMLElement>(
      'button, input, textarea, select, a[href], [tabindex]:not([tabindex="-1"])',
    ),
  ).filter((node) => !node.hasAttribute("disabled"));
  if (focusables.length === 0) return;

  const first = focusables[0];
  const last = focusables[focusables.length - 1];
  if (event.shiftKey && document.activeElement === first) {
    event.preventDefault();
    last.focus();
  } else if (!event.shiftKey && document.activeElement === last) {
    event.preventDefault();
    first.focus();
  }
}

function paintPin(): void {
  pinButton.classList.toggle("active", pinned);
  pinButton.title = pinned
    ? "常驻模式：保存后保持打开（点击切换）"
    : "临时模式：保存后自动收起（点击切换）";
}

function setSource(source: string | null): void {
  sourceLabel.textContent = source ? `来源：${source}` : "";
  sourceLabel.title = source ?? "";
}

async function refreshSettings(): Promise<void> {
  settings = await invoke<Settings>("get_settings");
  hotkeyInput.value = formatAccel(settings.hotkey);
  notesDirInput.value = settings.notesDir;
  captureSourceCheck.checked = settings.captureSource;
  markdownModeCheck.checked = settings.markdownMode;
}

async function hideWindow(): Promise<void> {
  closeSettings();
  closeHistory();
  closeWelcome();
  setBackgroundInert(false);
  await flushDraft();
  await invoke("hide_window");
}

async function saveNote(): Promise<void> {
  const content = textarea.value.trim();
  if (!content) {
    await hideWindow();
    return;
  }

  try {
    const saved = await invoke<SavedNote>("save_note", { content });
    textarea.value = "";
    if (draftTimer !== undefined) {
      window.clearTimeout(draftTimer);
      draftTimer = undefined;
    }
    localStorage.removeItem(DRAFT_KEY);
    void invoke("clear_draft").catch(() => {});
    toast(`已保存 · ${saved.time}`);
    if (!pinned) {
      window.setTimeout(() => {
        void invoke("hide_window");
      }, 380);
    }
  } catch (error) {
    toast(String(error), true);
  }
}

function openSettings(): void {
  closeHistory();
  closeWelcome();
  hotkeyInput.value = formatAccel(settings.hotkey);
  notesDirInput.value = settings.notesDir;
  captureSourceCheck.checked = settings.captureSource;
  markdownModeCheck.checked = settings.markdownMode;
  hotkeyError.textContent = "";
  settingsPanel.classList.remove("hidden");
  setBackgroundInert(true);
  settingsClose.focus();
}

function stopRecording(): void {
  recording = false;
  hotkeyRecord.classList.remove("recording");
  hotkeyRecord.textContent = "录制";
}

function closeSettings(): void {
  if (settingsPanel.classList.contains("hidden")) return;
  settingsPanel.classList.add("hidden");
  stopRecording();
  if (!historyPanel.classList.contains("hidden") || !welcomePanel.classList.contains("hidden")) {
    return;
  }
  setBackgroundInert(false);
  settingsButton.focus();
}

function dateLabel(date: string): string {
  const now = new Date();
  const today = [
    now.getFullYear(),
    String(now.getMonth() + 1).padStart(2, "0"),
    String(now.getDate()).padStart(2, "0"),
  ].join("-");
  return date === today ? `今天 · ${date}` : date;
}

function closeHistory(): void {
  if (historyPanel.classList.contains("hidden")) return;
  historyPanel.classList.add("hidden");
  if (!settingsPanel.classList.contains("hidden") || !welcomePanel.classList.contains("hidden")) {
    return;
  }
  setBackgroundInert(false);
  notesButton.focus();
}

function showWelcome(): void {
  welcomeHotkey.textContent = formatAccel(settings.hotkey);
  welcomeDir.textContent = settings.notesDir;
  welcomePanel.classList.remove("hidden");
  setBackgroundInert(true);
  welcomeStart.focus();
}

function closeWelcome(): void {
  if (welcomePanel.classList.contains("hidden")) return;
  welcomePanel.classList.add("hidden");
  settings.onboarded = true;
  void invoke("complete_onboarding").catch(() => {
    // 下次启动时重新展示引导
  });
  if (!settingsPanel.classList.contains("hidden") || !historyPanel.classList.contains("hidden")) {
    return;
  }
  setBackgroundInert(false);
}

async function dismissWelcome(openSettingsAfter = false): Promise<void> {
  closeWelcome();
  if (openSettingsAfter) {
    openSettings();
  } else {
    textarea.focus();
  }
}

function markdownHtml(markdown: string): string {
  const html = marked.parse(markdown, { async: false });
  return DOMPurify.sanitize(String(html));
}

function paintPreviewButton(): void {
  historyPreview.setAttribute("aria-pressed", String(settings.historyPreview));
  historyPreview.textContent = settings.historyPreview ? "原文" : "预览";
}

function bindDelete(button: HTMLButtonElement, note: NoteEntry): void {
  let confirmTimer: number | undefined;
  const reset = (): void => {
    if (confirmTimer !== undefined) window.clearTimeout(confirmTimer);
    confirmTimer = undefined;
    button.dataset.confirm = "";
    button.textContent = "删除";
    button.classList.remove("confirm");
  };

  button.addEventListener("click", () => {
    if (button.dataset.confirm === "1") {
      reset();
      void deleteEntry(note);
      return;
    }
    button.dataset.confirm = "1";
    button.textContent = "确认";
    button.classList.add("confirm");
    confirmTimer = window.setTimeout(reset, 3000);
  });
}

async function deleteEntry(note: NoteEntry): Promise<void> {
  try {
    await invoke("delete_note_entry", {
      path: note.path,
      date: note.date,
      time: note.time,
      content: note.content,
    });
    toast("已删除");
    refreshNotes();
  } catch (error) {
    toast(String(error), true);
  }
}

function startEdit(note: NoteEntry, contentEl: HTMLElement, editButton: HTMLButtonElement): void {
  if (contentEl.dataset.editing === "1") return;
  contentEl.dataset.editing = "1";

  const wrap = document.createElement("div");
  wrap.className = "entry-edit-wrap";

  const editor = document.createElement("textarea");
  editor.className = "entry-edit";
  editor.value = note.content;
  editor.rows = Math.min(10, Math.max(2, note.content.split("\n").length + 1));
  editor.setAttribute("aria-label", "编辑笔记内容");

  const actions = document.createElement("div");
  actions.className = "entry-edit-actions";

  const save = document.createElement("button");
  save.type = "button";
  save.className = "btn tiny primary";
  save.textContent = "保存";

  const cancel = document.createElement("button");
  cancel.type = "button";
  cancel.className = "btn tiny";
  cancel.textContent = "取消";

  const close = (): void => {
    wrap.replaceWith(contentEl);
    contentEl.dataset.editing = "";
    editButton.focus();
  };

  cancel.addEventListener("click", close);

  const commit = async (): Promise<void> => {
    const value = editor.value.trim();
    if (!value) {
      toast("内容不能为空", true);
      return;
    }
    if (value === note.content) {
      close();
      return;
    }
    save.disabled = true;
    try {
      await invoke("update_note_entry", {
        path: note.path,
        date: note.date,
        time: note.time,
        content: note.content,
        newContent: value,
      });
      toast("已更新");
      refreshNotes();
    } catch (error) {
      save.disabled = false;
      toast(String(error), true);
    }
  };

  save.addEventListener("click", () => void commit());
  editor.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      close();
      return;
    }
    if ((event.ctrlKey || event.metaKey) && event.key === "Enter") {
      event.preventDefault();
      void commit();
    }
  });

  actions.append(save, cancel);
  wrap.append(editor, actions);
  contentEl.replaceWith(wrap);
  editor.focus();
  editor.setSelectionRange(editor.value.length, editor.value.length);
}

function renderNotes(): void {
  const term = serverFiltered ? "" : historySearch.value.trim().toLowerCase();
  const filtered = term ? notes.filter((note) => matchesTerm(note, term)) : notes;

  entryList.replaceChildren();

  if (filtered.length === 0) {
    const empty = document.createElement("p");
    empty.className = "history-empty";
    empty.textContent =
      notes.length === 0 && !serverFiltered ? "还没有记录" : "没有匹配的笔记";
    entryList.append(empty);
  } else {
    let lastDate = "";
    for (const note of filtered) {
      if (note.date !== lastDate) {
        lastDate = note.date;
        const date = document.createElement("div");
        date.className = "entry-date";
        date.textContent = dateLabel(note.date);
        entryList.append(date);
      }

      const entry = document.createElement("article");
      entry.className = "entry";

      const head = document.createElement("div");
      head.className = "entry-head";

      const time = document.createElement("span");
      time.className = "entry-time";
      time.textContent = note.time;
      head.append(time);

      if (note.source) {
        const source = document.createElement("span");
        source.className = "entry-source";
        source.textContent = `来源：${note.source}`;
        head.append(source);
      }

      const actions = document.createElement("div");
      actions.className = "entry-actions";

      const editButton = document.createElement("button");
      editButton.type = "button";
      editButton.className = "entry-action";
      editButton.textContent = "编辑";
      editButton.title = "编辑这条笔记";

      const deleteButton = document.createElement("button");
      deleteButton.type = "button";
      deleteButton.className = "entry-action danger";
      deleteButton.textContent = "删除";
      deleteButton.title = "删除这条笔记";

      actions.append(editButton, deleteButton);

      const openButton = document.createElement("button");
      openButton.type = "button";
      openButton.className = "entry-open";
      openButton.title = "在资源管理器中定位";
      openButton.setAttribute("aria-label", "在资源管理器中定位");
      openButton.textContent = "↗";
      openButton.addEventListener("click", () => {
        void invoke("open_note_file", { path: note.path }).catch((error) =>
          toast(String(error), true),
        );
      });

      head.append(actions, openButton);

      const content = document.createElement("div");
      content.className = "entry-content";
      if (settings.historyPreview) {
        content.classList.add("markdown");
        content.innerHTML = markdownHtml(note.content);
      } else {
        content.textContent = note.content;
      }

      editButton.addEventListener("click", () => startEdit(note, content, editButton));
      bindDelete(deleteButton, note);

      entry.append(head, content);
      entryList.append(entry);
    }

    if (!serverFiltered && hasMoreNotes) {
      const more = document.createElement("button");
      more.type = "button";
      more.className = "btn load-more";
      more.textContent = "加载更多";
      more.addEventListener("click", () => {
        more.disabled = true;
        more.textContent = "加载中…";
        void loadNotes(false);
      });
      entryList.append(more);
    }
  }

  if (serverFiltered) {
    historyCount.textContent =
      filtered.length >= SEARCH_LIMIT
        ? `前 ${filtered.length} 条结果`
        : `${filtered.length} 条结果`;
  } else if (term) {
    historyCount.textContent = `${filtered.length} / ${notes.length} 条`;
  } else {
    historyCount.textContent = hasMoreNotes ? `已显示 ${notes.length} 条` : `共 ${notes.length} 条`;
  }
}

async function loadNotes(reset = true): Promise<void> {
  const seq = ++searchSeq;
  if (reset) noteOffset = 0;

  try {
    const batch = await invoke<NoteEntry[]>("list_notes", {
      limit: PAGE_SIZE,
      offset: noteOffset,
    });
    if (seq !== searchSeq) return;
    notes = reset ? batch : [...notes, ...batch];
    noteOffset = notes.length;
    hasMoreNotes = batch.length === PAGE_SIZE;
    serverFiltered = false;
  } catch (error) {
    if (seq !== searchSeq) return;
    if (reset) notes = [];
    hasMoreNotes = false;
    toast(String(error), true);
  }
  renderNotes();
  if (reset) entryList.scrollTop = 0;
}

async function runSearch(): Promise<void> {
  const seq = ++searchSeq;
  const term = historySearch.value.trim();
  if (!term) {
    serverFiltered = false;
    await loadNotes(true);
    return;
  }

  try {
    const results = await invoke<NoteEntry[]>("search_notes", {
      query: term,
      limit: SEARCH_LIMIT,
    });
    if (seq !== searchSeq) return;
    notes = results;
    noteOffset = 0;
    hasMoreNotes = false;
    serverFiltered = true;
  } catch (error) {
    if (seq !== searchSeq) return;
    notes = [];
    serverFiltered = true;
    toast(String(error), true);
  }
  renderNotes();
  entryList.scrollTop = 0;
}

function refreshNotes(): void {
  if (historySearch.value.trim()) {
    void runSearch();
  } else {
    void loadNotes(true);
  }
}

async function toggleHistoryPreview(): Promise<void> {
  const next = !settings.historyPreview;
  settings.historyPreview = next;
  paintPreviewButton();
  renderNotes();
  try {
    await invoke("update_history_preview", { enabled: next });
  } catch (error) {
    settings.historyPreview = !next;
    paintPreviewButton();
    renderNotes();
    toast(String(error), true);
  }
}

function openHistory(): void {
  closeSettings();
  closeWelcome();
  historyPanel.classList.remove("hidden");
  setBackgroundInert(true);
  historySearch.value = "";
  notes = [];
  serverFiltered = false;
  noteOffset = 0;
  hasMoreNotes = false;
  paintPreviewButton();
  entryList.replaceChildren();
  historyCount.textContent = "";
  historySearch.focus();
  void loadNotes(true);
}

function startRecording(): void {
  recording = true;
  hotkeyRecord.classList.add("recording");
  hotkeyRecord.textContent = "按键…";
  hotkeyError.textContent = "";
}

async function applyHotkey(accel: string): Promise<void> {
  try {
    await invoke("update_hotkey", { accel });
    await refreshSettings();
    stopRecording();
    toast("快捷键已更新");
  } catch (error) {
    hotkeyError.textContent = String(error);
    stopRecording();
  }
}

function handleRecording(event: KeyboardEvent): void {
  event.preventDefault();
  event.stopPropagation();

  if (event.key === "Escape") {
    stopRecording();
    return;
  }

  const accel = accelFromEvent(event);
  if (accel) void applyHotkey(accel);
}

async function init(): Promise<void> {
  try {
    await refreshSettings();
    pinned = settings.pinned;
    paintPin();
    paintPreviewButton();

    try {
      autostartCheck.checked = await isEnabled();
    } catch {
      autostartCheck.disabled = true;
    }

    // 草稿优先读磁盘；首次升级时把 localStorage 中的旧草稿迁移过来
    const legacyDraft = localStorage.getItem(DRAFT_KEY);
    const draft = await invoke<string>("load_draft");
    if (draft) {
      textarea.value = draft;
    } else if (legacyDraft) {
      textarea.value = legacyDraft;
      await invoke("save_draft", { content: legacyDraft }).catch(() => {});
    }
    if (legacyDraft !== null) localStorage.removeItem(DRAFT_KEY);

    appReady = true;

    // 窗口在启动时就可见（首次运行、快捷键冲突）时立即提示；否则留到第一次唤起
    if (await getCurrentWindow().isVisible()) {
      if (await handleFirstShow()) return;
    }

    textarea.focus();
  } catch (error) {
    toast(String(error), true);
  }
}

async function handleFirstShow(): Promise<boolean> {
  if (await showStartupNotice()) return true;
  if (!settings.onboarded) {
    showWelcome();
    return true;
  }
  return false;
}

let startupNoticeShown = false;

/** 取出启动提示并展示；没有提示或已经展示过则返回 false。 */
async function showStartupNotice(): Promise<boolean> {
  if (startupNoticeShown) return false;

  const notice = await invoke<StartupNotice | null>("take_startup_notice");
  if (!notice) return false;

  startupNoticeShown = true;
  toast(notice.message, true, 5000);
  if (notice.openSettings) openSettings();
  return true;
}

textarea.addEventListener("input", scheduleDraftSave);

welcomeStart.addEventListener("click", () => void dismissWelcome());
welcomeSettings.addEventListener("click", () => void dismissWelcome(true));

pinButton.addEventListener("click", async () => {
  pinned = !pinned;
  paintPin();
  try {
    await invoke("set_pinned", { pinned });
    toast(pinned ? "常驻模式：保存后保持打开" : "保存后自动收起");
  } catch (error) {
    toast(String(error), true);
  }
});

settingsButton.addEventListener("click", openSettings);
settingsClose.addEventListener("click", closeSettings);
notesButton.addEventListener("click", openHistory);
historyClose.addEventListener("click", closeHistory);
historyPreview.addEventListener("click", () => void toggleHistoryPreview());
historySearch.addEventListener("input", () => {
  if (searchTimer !== undefined) window.clearTimeout(searchTimer);
  searchTimer = window.setTimeout(() => {
    searchTimer = undefined;
    void runSearch();
  }, 250);
});
historyFolder.addEventListener("click", () => {
  void invoke("open_notes_dir").catch((error) => toast(String(error), true));
});
closeButton.addEventListener("click", () => void hideWindow());

hotkeyRecord.addEventListener("click", () => {
  if (recording) {
    stopRecording();
  } else {
    startRecording();
  }
});

chooseDirButton.addEventListener("click", async () => {
  try {
    const selected = await open({
      directory: true,
      multiple: false,
      title: "选择笔记目录",
      defaultPath: settings.notesDir || undefined,
    });
    if (typeof selected === "string") {
      await invoke("update_notes_dir", { path: selected });
      await refreshSettings();
      toast("笔记目录已更新");
    }
  } catch (error) {
    toast(String(error), true);
  }
});

captureSourceCheck.addEventListener("change", async () => {
  try {
    await invoke("update_capture_source", {
      enabled: captureSourceCheck.checked,
    });
    await refreshSettings();
  } catch (error) {
    captureSourceCheck.checked = !captureSourceCheck.checked;
    toast(String(error), true);
  }
});

markdownModeCheck.addEventListener("change", async () => {
  try {
    await invoke("update_markdown_mode", {
      enabled: markdownModeCheck.checked,
    });
    await refreshSettings();
    toast(markdownModeCheck.checked ? "已开启 Markdown 模式" : "已关闭 Markdown 模式");
  } catch (error) {
    markdownModeCheck.checked = !markdownModeCheck.checked;
    toast(String(error), true);
  }
});

autostartCheck.addEventListener("change", async () => {
  try {
    if (autostartCheck.checked) {
      await enable();
    } else {
      await disable();
    }
    toast(autostartCheck.checked ? "已开启开机自启" : "已关闭开机自启");
  } catch (error) {
    autostartCheck.checked = !autostartCheck.checked;
    toast(String(error), true);
  }
});

window.addEventListener(
  "keydown",
  (event) => {
    if (recording) {
      handleRecording(event);
      return;
    }

    trapFocus(event);

    // 条目编辑框自己处理 Esc / Ctrl+Enter，避免误关整个面板
    if ((event.target as HTMLElement | null)?.closest(".entry-edit")) return;

    if (event.key === "Escape") {
      event.preventDefault();
      if (!welcomePanel.classList.contains("hidden")) {
        void dismissWelcome();
      } else if (!historyPanel.classList.contains("hidden")) {
        closeHistory();
      } else if (!settingsPanel.classList.contains("hidden")) {
        closeSettings();
      } else {
        void hideWindow();
      }
      return;
    }

    if ((event.ctrlKey || event.metaKey) && event.key === "Enter" && !event.isComposing) {
      event.preventDefault();
      void saveNote();
      return;
    }

    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
      event.preventDefault();
      void saveNote();
    }
  },
  true,
);

resizeGrip.addEventListener("pointerdown", (event) => {
  event.preventDefault();
  void getCurrentWindow().startResizeDragging("SouthEast");
});

void listen<ShowPayload>("window-shown", (event) => {
  setSource(event.payload.source);
  if (!appReady) return;
  if (!historyPanel.classList.contains("hidden")) {
    refreshNotes();
    return;
  }
  if (!settingsPanel.classList.contains("hidden")) return;
  void handleFirstShow().then((handled) => {
    if (!handled) textarea.focus();
  });
});

void listen("open-settings", () => {
  openSettings();
});

void listen("open-history", () => {
  openHistory();
});

void init();
