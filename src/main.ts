import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";
import { disable, enable, isEnabled } from "@tauri-apps/plugin-autostart";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import DOMPurify from "dompurify";
import { marked } from "marked";
import {
  accelFromEvent,
  clampOpacity,
  formatAccel,
  matchesTerm,
  normalizeTheme,
  resolveTheme,
  type NoteEntry,
  type ThemeMode,
} from "./lib";

interface Settings {
  notesDir: string;
  hotkey: string;
  pinned: boolean;
  captureSource: boolean;
  markdownMode: boolean;
  historyPreview: boolean;
  autoUpdate: boolean;
  onboarded: boolean;
  theme: ThemeMode;
  autoHideOnBlur: boolean;
  backgroundOpacity: number;
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
const autoUpdateCheck = el<HTMLInputElement>("auto-update");
const autoHideCheck = el<HTMLInputElement>("auto-hide");
const themeSelect = el<HTMLSelectElement>("theme");
const bgOpacityInput = el<HTMLInputElement>("bg-opacity");
const bgOpacityValue = el<HTMLSpanElement>("bg-opacity-value");
const checkUpdateButton = el<HTMLButtonElement>("check-update");
const updateStatus = el<HTMLParagraphElement>("update-status");
const appVersionLabel = el<HTMLSpanElement>("app-version");
const updateBanner = el<HTMLDivElement>("update-banner");
const updateBannerText = el<HTMLSpanElement>("update-banner-text");
const updateBannerAction = el<HTMLButtonElement>("update-banner-action");
const updateBannerDismiss = el<HTMLButtonElement>("update-banner-dismiss");
const sourceLabel = el<HTMLSpanElement>("source");
const card = el<HTMLDivElement>("card");
const toastEl = el<HTMLDivElement>("toast");
const toastMessage = el<HTMLSpanElement>("toast-message");
const toastAction = el<HTMLButtonElement>("toast-action");
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
  autoUpdate: true,
  onboarded: true,
  theme: "system",
  autoHideOnBlur: false,
  backgroundOpacity: 1,
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
let pendingUpdate: Update | null = null;
let updateReady = false;
let updating = false;

const PAGE_SIZE = 100;
const SEARCH_LIMIT = 200;

let toastActionHandler: (() => void) | null = null;

function hideToast(): void {
  if (toastTimer !== undefined) {
    window.clearTimeout(toastTimer);
    toastTimer = undefined;
  }
  toastEl.classList.remove("show", "actionable");
  toastAction.classList.add("hidden");
  toastActionHandler = null;
}

function toast(message: string, error = false, duration = 1600): void {
  toastEl.setAttribute("role", error ? "alert" : "status");
  toastEl.setAttribute("aria-live", error ? "assertive" : "polite");
  toastMessage.textContent = message;
  toastEl.classList.toggle("error", error);
  toastEl.classList.remove("actionable");
  toastAction.classList.add("hidden");
  toastActionHandler = null;
  toastEl.classList.add("show");
  if (toastTimer !== undefined) window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => toastEl.classList.remove("show"), duration);
}

/** 带一个操作按钮的 toast（目前用于「撤销删除」），显示时长由调用方控制。 */
function showActionToast(message: string, label: string, onAction: () => void): void {
  toastEl.setAttribute("role", "status");
  toastEl.setAttribute("aria-live", "polite");
  toastMessage.textContent = message;
  toastEl.classList.remove("error");
  toastAction.textContent = label;
  toastAction.classList.remove("hidden");
  toastActionHandler = onAction;
  toastEl.classList.add("actionable", "show");
  if (toastTimer !== undefined) {
    window.clearTimeout(toastTimer);
    toastTimer = undefined;
  }
}

toastAction.addEventListener("click", () => {
  const handler = toastActionHandler;
  hideToast();
  handler?.();
});

function showUpdateBanner(message: string, actionLabel: string | null): void {
  updateBannerText.textContent = message;
  updateBannerAction.hidden = actionLabel === null;
  if (actionLabel) updateBannerAction.textContent = actionLabel;
  updateBanner.classList.remove("hidden");
}

function hideUpdateBanner(): void {
  updateBanner.classList.add("hidden");
}

let autoCheckTimeout: number | undefined;
let autoCheckInterval: number | undefined;

function stopAutoUpdateChecks(): void {
  if (autoCheckTimeout !== undefined) window.clearTimeout(autoCheckTimeout);
  if (autoCheckInterval !== undefined) window.clearInterval(autoCheckInterval);
  autoCheckTimeout = undefined;
  autoCheckInterval = undefined;
}

// 启动 5 秒后检查一次；常驻不关的实例每 6 小时再查一次
function startAutoUpdateChecks(): void {
  stopAutoUpdateChecks();
  if (!settings.autoUpdate) return;
  autoCheckTimeout = window.setTimeout(() => {
    autoCheckTimeout = undefined;
    void checkForUpdate(false);
    autoCheckInterval = window.setInterval(
      () => void checkForUpdate(false),
      6 * 60 * 60 * 1000,
    );
  }, 5000);
}

async function checkForUpdate(manual: boolean): Promise<void> {
  if (updating) return;
  // 已下载安装但尚未重启时，再查一次仍会返回同一版本，直接提示重启即可
  if (updateReady) {
    if (manual) {
      updateStatus.textContent = "更新已安装，重启后生效";
      showUpdateBanner("更新已安装，重启后生效", "重启");
    }
    return;
  }
  if (manual) {
    checkUpdateButton.disabled = true;
    updateStatus.textContent = "正在检查更新…";
  }

  try {
    const update = await check();
    if (!update) {
      if (manual) {
        updateStatus.textContent = "已是最新版本";
        toast("已是最新版本");
      }
      return;
    }

    pendingUpdate = update;
    updateStatus.textContent = `发现新版本 ${update.version}`;
    showUpdateBanner(`发现新版本 ${update.version}`, "更新");
  } catch (error) {
    // 启动时的静默检查失败不打扰用户；手动检查才提示
    if (manual) {
      updateStatus.textContent = "检查更新失败，请稍后再试";
      toast(String(error), true);
    }
  } finally {
    if (manual) checkUpdateButton.disabled = false;
  }
}

async function installUpdate(): Promise<void> {
  const update = pendingUpdate;
  if (!update || updating) return;

  updating = true;
  updateBannerAction.disabled = true;
  showUpdateBanner("正在下载更新…", null);

  try {
    let total = 0;
    let received = 0;
    await update.downloadAndInstall((event) => {
      if (event.event === "Started") {
        total = event.data.contentLength ?? 0;
      } else if (event.event === "Progress") {
        received += event.data.chunkLength;
        const percent =
          total > 0 ? Math.min(100, Math.round((received / total) * 100)) : null;
        showUpdateBanner(
          percent === null ? "正在下载更新…" : `正在下载更新… ${percent}%`,
          null,
        );
      }
    });

    updateReady = true;
    updateStatus.textContent = `已安装 ${update.version}，重启后生效`;
    showUpdateBanner(`新版本 ${update.version} 已安装，重启后生效`, "重启");
  } catch (error) {
    showUpdateBanner("更新失败，请稍后重试", "重试");
    updateStatus.textContent = "更新失败";
    toast(String(error), true);
  } finally {
    updating = false;
    updateBannerAction.disabled = false;
  }
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

/**
 * 面板打开时让背景不可聚焦，关闭后恢复。
 * #toast 不参与：撤销删除的按钮要能在面板打开时点击。
 */
function setBackgroundInert(inert: boolean): void {
  for (const selector of ["header", "main", "footer", "#resize-grip"]) {
    const node = document.querySelector<HTMLElement>(selector);
    if (node) node.inert = inert;
  }
}

function anyPanelOpen(): boolean {
  return [welcomePanel, settingsPanel, historyPanel].some(
    (panel) => !panel.classList.contains("hidden"),
  );
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

const panelLeaveTimers = new WeakMap<HTMLElement, number>();

/** 打开面板：播放入场动画，并让背景让位（半透明时避免正文透底）。 */
function showPanel(panel: HTMLElement): void {
  const pending = panelLeaveTimers.get(panel);
  if (pending !== undefined) {
    window.clearTimeout(pending);
    panelLeaveTimers.delete(panel);
  }
  panel.classList.remove("hidden", "panel-leave", "panel-enter");
  // 强制回流，连续打开同一面板时动画也会重新播放
  void panel.offsetWidth;
  if (!reducedMotion.matches) {
    panel.classList.add("panel-enter");
    window.setTimeout(() => panel.classList.remove("panel-enter"), PANEL_IN_MS);
  }
  card.classList.add("panel-open");
  setBackgroundInert(true);
}

/** 关闭面板：动画结束后才真正 hidden，焦点与 inert 也在那时恢复。 */
function hidePanel(panel: HTMLElement, animate = true): void {
  if (panel.classList.contains("hidden") || panel.classList.contains("panel-leave")) return;
  panel.classList.remove("panel-enter");

  if (!animate || reducedMotion.matches) {
    panel.classList.add("hidden");
    afterPanelClosed();
    return;
  }

  panel.classList.add("panel-leave");
  const timer = window.setTimeout(() => {
    panelLeaveTimers.delete(panel);
    // 动画期间被重新打开：showPanel 已移除 panel-leave，不要再隐藏
    if (!panel.classList.contains("panel-leave")) return;
    panel.classList.remove("panel-leave");
    panel.classList.add("hidden");
    afterPanelClosed();
  }, PANEL_OUT_MS);
  panelLeaveTimers.set(panel, timer);
}

/** 没有面板可见时恢复背景交互，焦点回到输入框而不是标题栏按钮。 */
function afterPanelClosed(): void {
  if (anyPanelOpen()) return;
  card.classList.remove("panel-open");
  setBackgroundInert(false);
  textarea.focus();
}

const systemLight = window.matchMedia("(prefers-color-scheme: light)");
const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");
// 与 styles.css 里的动画时长保持一致
const PANEL_IN_MS = 160;
const PANEL_OUT_MS = 120;

function applyTheme(mode: ThemeMode): void {
  const resolved = resolveTheme(mode, systemLight.matches);
  document.documentElement.dataset.theme = resolved;
  document.documentElement.style.colorScheme = resolved;
}

function paintOpacity(value: number): void {
  document.documentElement.style.setProperty("--surface-alpha", String(value));
  bgOpacityInput.value = String(Math.round(value * 100));
  bgOpacityValue.textContent = `${Math.round(value * 100)}%`;
}

let windowFocusedOnce = false;
let suppressBlurUntil = 0;

/** 可选：窗口失去焦点时收起。常驻、面板打开、快捷键录制期间都不触发。 */
function registerAutoHide(): void {
  void getCurrentWindow().onFocusChanged(async ({ payload: focused }) => {
    if (focused) {
      windowFocusedOnce = true;
      return;
    }
    if (!windowFocusedOnce || !settings.autoHideOnBlur || pinned || recording) return;
    if (Date.now() < suppressBlurUntil || anyPanelOpen()) return;
    if (!(await getCurrentWindow().isVisible())) return;

    // 再确认一次，避开切换焦点时的瞬时抖动
    window.setTimeout(() => {
      if (Date.now() < suppressBlurUntil) return;
      if (document.hasFocus() || pinned || recording || anyPanelOpen()) return;
      void hideWindow();
    }, 120);
  });
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
  autoUpdateCheck.checked = settings.autoUpdate;
  autoHideCheck.checked = settings.autoHideOnBlur;

  settings.theme = normalizeTheme(settings.theme);
  themeSelect.value = settings.theme;
  applyTheme(settings.theme);

  settings.backgroundOpacity = clampOpacity(settings.backgroundOpacity);
  paintOpacity(settings.backgroundOpacity);
}

async function hideWindow(): Promise<void> {
  // 收起窗口时不做退出动画，避免面板还停留在一半
  closeSettings(false);
  closeHistory(false);
  closeWelcome(false);
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
  closeHistory(false);
  closeWelcome(false);
  hotkeyInput.value = formatAccel(settings.hotkey);
  notesDirInput.value = settings.notesDir;
  captureSourceCheck.checked = settings.captureSource;
  markdownModeCheck.checked = settings.markdownMode;
  autoUpdateCheck.checked = settings.autoUpdate;
  autoHideCheck.checked = settings.autoHideOnBlur;
  themeSelect.value = settings.theme;
  paintOpacity(settings.backgroundOpacity);
  hotkeyError.textContent = "";
  showPanel(settingsPanel);
  settingsClose.focus();
}

function stopRecording(): void {
  recording = false;
  hotkeyRecord.classList.remove("recording");
  hotkeyRecord.textContent = "录制";
}

function closeSettings(animate = true): void {
  if (settingsPanel.classList.contains("hidden")) return;
  stopRecording();
  hidePanel(settingsPanel, animate);
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

function closeHistory(animate = true): void {
  if (historyPanel.classList.contains("hidden")) return;
  // 面板关掉后列表就不再可见，未落盘的删除要立刻提交
  void flushPendingDelete();
  hidePanel(historyPanel, animate);
}

function showWelcome(): void {
  welcomeHotkey.textContent = formatAccel(settings.hotkey);
  welcomeDir.textContent = settings.notesDir;
  showPanel(welcomePanel);
  welcomeStart.focus();
}

function closeWelcome(animate = true): void {
  if (welcomePanel.classList.contains("hidden")) return;
  settings.onboarded = true;
  void invoke("complete_onboarding").catch(() => {
    // 下次启动时重新展示引导
  });
  hidePanel(welcomePanel, animate);
}

async function dismissWelcome(openSettingsAfter = false): Promise<void> {
  // 直接切到设置时不做退出动画，避免两个面板叠在一起
  closeWelcome(!openSettingsAfter);
  if (openSettingsAfter) openSettings();
}

function markdownHtml(markdown: string): string {
  const html = marked.parse(markdown, { async: false });
  return DOMPurify.sanitize(String(html));
}

function paintPreviewButton(): void {
  historyPreview.setAttribute("aria-pressed", String(settings.historyPreview));
  historyPreview.textContent = settings.historyPreview ? "原文" : "预览";
}

interface PendingDelete {
  note: NoteEntry;
  index: number;
  timer: number;
}

let pendingDelete: PendingDelete | null = null;

function bindDelete(button: HTMLButtonElement, note: NoteEntry): void {
  button.addEventListener("click", () => requestDelete(note));
}

/** 先从列表移除并给 5 秒撤销窗口，到期才真正写盘。 */
function requestDelete(note: NoteEntry): void {
  // 同时只保留一条待删除记录，前一条立即落盘
  if (pendingDelete) void commitPendingDelete();

  const index = notes.indexOf(note);
  if (index < 0) return;
  notes.splice(index, 1);
  renderNotes();

  const timer = window.setTimeout(() => void commitPendingDelete(), 5000);
  pendingDelete = { note, index, timer };
  showActionToast("已删除", "撤销", undoDelete);
}

async function commitPendingDelete(): Promise<void> {
  const pending = pendingDelete;
  if (!pending) return;
  pendingDelete = null;
  window.clearTimeout(pending.timer);
  // 只关掉自己的撤销提示，别误伤期间的其他 toast
  if (toastActionHandler === undoDelete) hideToast();

  try {
    await invoke("delete_note_entry", {
      path: pending.note.path,
      date: pending.note.date,
      time: pending.note.time,
      content: pending.note.content,
    });
  } catch (error) {
    // 删除失败就把条目放回原位，避免界面上「看起来删掉了」
    notes.splice(Math.min(pending.index, notes.length), 0, pending.note);
    renderNotes();
    toast(String(error), true);
  }
}

function undoDelete(): void {
  const pending = pendingDelete;
  if (!pending) return;
  pendingDelete = null;
  window.clearTimeout(pending.timer);
  notes.splice(Math.min(pending.index, notes.length), 0, pending.note);
  renderNotes();
  toast("已恢复", false, 1200);
}

async function flushPendingDelete(): Promise<void> {
  if (pendingDelete) await commitPendingDelete();
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
  await flushPendingDelete();
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
  // 重新查询前先提交待删除项，否则磁盘上的旧条目会重新出现
  await flushPendingDelete();
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
  closeSettings(false);
  closeWelcome(false);
  showPanel(historyPanel);
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
    registerAutoHide();

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

    try {
      appVersionLabel.textContent = `v${await invoke<string>("app_version")}`;
    } catch {
      appVersionLabel.textContent = "";
    }

    // 启动后延迟检查，不阻塞窗口显示；失败时静默
    startAutoUpdateChecks();

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
settingsClose.addEventListener("click", () => closeSettings());
notesButton.addEventListener("click", openHistory);
historyClose.addEventListener("click", () => closeHistory());
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

autoUpdateCheck.addEventListener("change", async () => {
  try {
    await invoke("update_auto_update", { enabled: autoUpdateCheck.checked });
    await refreshSettings();
    startAutoUpdateChecks();
    toast(autoUpdateCheck.checked ? "已开启自动检查更新" : "已关闭自动检查更新");
  } catch (error) {
    autoUpdateCheck.checked = !autoUpdateCheck.checked;
    toast(String(error), true);
  }
});

autoHideCheck.addEventListener("change", async () => {
  try {
    await invoke("update_auto_hide", { enabled: autoHideCheck.checked });
    settings.autoHideOnBlur = autoHideCheck.checked;
    toast(autoHideCheck.checked ? "已开启失焦收起" : "已关闭失焦收起");
  } catch (error) {
    autoHideCheck.checked = !autoHideCheck.checked;
    toast(String(error), true);
  }
});

themeSelect.addEventListener("change", async () => {
  const previous = settings.theme;
  const next = normalizeTheme(themeSelect.value);
  settings.theme = next;
  applyTheme(next);
  try {
    settings.theme = normalizeTheme(await invoke<string>("update_theme", { theme: next }));
    themeSelect.value = settings.theme;
    applyTheme(settings.theme);
    toast("主题已更新");
  } catch (error) {
    settings.theme = previous;
    themeSelect.value = previous;
    applyTheme(previous);
    toast(String(error), true);
  }
});

bgOpacityInput.addEventListener("input", () => {
  paintOpacity(clampOpacity(Number(bgOpacityInput.value) / 100));
});

bgOpacityInput.addEventListener("change", async () => {
  const previous = settings.backgroundOpacity;
  const value = clampOpacity(Number(bgOpacityInput.value) / 100);
  try {
    const applied = await invoke<number>("update_background_opacity", { opacity: value });
    settings.backgroundOpacity = clampOpacity(applied);
    paintOpacity(settings.backgroundOpacity);
  } catch (error) {
    settings.backgroundOpacity = previous;
    paintOpacity(previous);
    toast(String(error), true);
  }
});

systemLight.addEventListener("change", () => {
  if (settings.theme === "system") applyTheme("system");
});

// 窗口被隐藏时把未落盘的删除提交掉（例如从托盘收起）
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "hidden") void flushPendingDelete();
});

checkUpdateButton.addEventListener("click", () => {
  void checkForUpdate(true);
});

updateBannerAction.addEventListener("click", () => {
  if (updateReady) {
    void relaunch();
  } else {
    void installUpdate();
  }
});

updateBannerDismiss.addEventListener("click", hideUpdateBanner);

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
  // 唤起瞬间可能有一次焦点切换，避免误触发失焦收起
  suppressBlurUntil = Date.now() + 600;
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
  suppressBlurUntil = Date.now() + 600;
  openSettings();
});

void listen("open-history", () => {
  suppressBlurUntil = Date.now() + 600;
  openHistory();
});

void init();
