import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { externalLinkTarget, type NoteEntry } from "./lib";
import { ui } from "./dom";
import { toast } from "./toast";
import { anyPanelOpen, setBackgroundInert, trapFocus } from "./panels";
import { refreshSettings, settings } from "./settings-store";
import {
  clearStoredDraft,
  flushDraft,
  restoreDraft,
  scheduleDraftSave,
} from "./draft";
import { focusEditor, getText, initEditor, setText, toggleSourceMode } from "./editor";
import {
  activeContinuation,
  endContinuation,
  initContinuation,
  refreshContinuationContent,
  startContinuation,
  updateContinuationContent,
} from "./continuation";
import { initSync, markSyncPending, refreshSyncStatus } from "./sync";
import { initUpdater, startAutoUpdateChecks } from "./updater";
import {
  closeHistory,
  hasOpenEditor,
  initHistory,
  paintPreviewButton,
  refreshNotes,
  showHistoryPanel,
  suspendHistoryForLock,
} from "./history";
import {
  closeSettings,
  handleRecordingKey,
  initAutostart,
  initSettingsPanel,
  isRecording,
  showSettingsPanel,
} from "./settings-panel";
import {
  applyLockStatus,
  enterLockedMode,
  getLockStatus,
  initAppLock,
  isAppLocked,
  lockNow,
  paintLockButton,
} from "./app-lock";
import { initLockSettings } from "./lock-settings";
import { closeWelcome, showWelcomePanel } from "./welcome";

interface SavedNote {
  path: string;
  time: string;
}

interface ShowPayload {
  source: string | null;
  locked: boolean;
}

interface StartupNotice {
  message: string;
  openSettings: boolean;
}

let pinned = false;
let saving = false;
let appReady = false;
let windowFocusedOnce = false;
let suppressBlurUntil = 0;

/** 可选：窗口失去焦点时收起。常驻、面板打开、快捷键录制期间都不触发。 */
function registerAutoHide(): void {
  getCurrentWindow()
    .onFocusChanged(async ({ payload: focused }) => {
      if (focused) {
        windowFocusedOnce = true;
        return;
      }
      if (!windowFocusedOnce || !settings.autoHideOnBlur || pinned || isRecording()) return;
      if (isAppLocked()) return;
      if (Date.now() < suppressBlurUntil || anyPanelOpen()) return;
      if (!(await getCurrentWindow().isVisible())) return;

      // 再确认一次，避开切换焦点时的瞬时抖动
      window.setTimeout(() => {
        if (Date.now() < suppressBlurUntil) return;
        if (document.hasFocus() || pinned || isRecording() || anyPanelOpen()) return;
        void hideWindow();
      }, 120);
    })
    .catch((error) => console.error("监听焦点变化失败", error));
}

function paintPin(): void {
  ui.pinButton.classList.toggle("active", pinned);
  ui.pinButton.title = pinned
    ? "常驻模式：保存后保持打开（点击切换）"
    : "临时模式：保存后自动收起（点击切换）";
}

function setSource(source: string | null): void {
  ui.sourceLabel.textContent = source ? `来源：${source}` : "";
  ui.sourceLabel.title = source ?? "";
}

async function hideWindow(): Promise<void> {
  if (isAppLocked()) {
    // 锁屏时草稿与面板命令都被后端拦截；只隐藏窗口，解锁状态保持不变。
    await invoke("hide_window");
    return;
  }
  // 收起窗口时不做退出动画，避免面板还停留在一半
  closeSettings(false);
  // 收起窗口是明确的「离开」动作：编辑框内容随窗口一起收起，不阻塞热键。
  closeHistory(false, true);
  closeWelcome(false);
  setBackgroundInert(false);
  await flushDraft();
  await invoke("hide_window");
}

interface SaveOptions {
  /** 忽略续写目标，强制作为新记录追加（用于「续写」前保存当前草稿） */
  forceAppend?: boolean;
  /** 成功后是否自动收起窗口，默认跟随常驻模式 */
  hideOnSuccess?: boolean;
  /** 不弹成功提示（调用方会给出合并后的提示） */
  quiet?: boolean;
}

interface SaveResult {
  ok: boolean;
  /** 保存期间编辑器里又出现新输入：调用方不要覆盖当前内容 */
  editorChanged: boolean;
}

/**
 * 保存成功后的统一收尾：编辑器内容没变就清空并结束续写，否则保留新输入。
 * 三条保存路径共用，避免提示与收起逻辑各写一份后逐渐漂移。
 */
function settleSave(raw: string, message: string, options: SaveOptions): SaveResult {
  // 保存期间用户可能继续输入：只有内容没变时才清空，否则保留新内容并补一次草稿
  if (getText() === raw) {
    setText("");
    clearStoredDraft();
    endContinuation();
    if (!options.quiet) toast(message);
    if (options.hideOnSuccess !== false && !pinned) {
      window.setTimeout(() => {
        void invoke("hide_window");
      }, 380);
    }
    return { ok: true, editorChanged: false };
  }

  scheduleDraftSave();
  if (!options.quiet) toast(`${message}（新输入尚未保存）`, true);
  return { ok: true, editorChanged: true };
}

async function saveNote(options: SaveOptions = {}): Promise<SaveResult> {
  if (saving || isAppLocked()) return { ok: false, editorChanged: false };

  // 与 target.content 比较用原始文本：正文本身可能以空白开头（外部编辑/同步写入），
  // trim 后比较会把「未修改」误判成修改并静默改写原记录。
  const raw = getText();
  const content = raw.trim();
  if (!content) {
    endContinuation();
    await hideWindow();
    return { ok: false, editorChanged: false };
  }

  saving = true;
  const target = options.forceAppend ? null : activeContinuation();
  try {
    if (target) {
      // 正文没有变化：不写盘，直接结束续写
      if (raw === target.content) {
        return settleSave(raw, `未修改 · ${target.time}`, options);
      }

      const update = () =>
        invoke<string[]>("update_note_entry", {
          path: target.path,
          date: target.date,
          time: target.time,
          content: target.content,
          newContent: content,
        });
      try {
        await update();
      } catch (error) {
        // 原记录可能已被历史面板、外部编辑器或同步改写：刷新定位依据后重试一次
        if (!(await refreshContinuationContent(target))) throw error;
        await update();
      }
      // 已经在磁盘上产生变更，先标记待同步；真正的同步结果由 sync-status 广播回来。
      markSyncPending();
      // 下一次保存必须用新正文定位原记录
      updateContinuationContent(content);
      return settleSave(raw, `已更新 · ${target.time}`, options);
    }

    const saved = await invoke<SavedNote>("save_note", { content });
    // 已经在磁盘上产生变更，先标记待同步；真正的同步结果由 sync-status 广播回来。
    markSyncPending();
    // 当前内容已作为新记录落盘，之前可能存在的续写目标随之作废
    endContinuation();
    return settleSave(raw, `已保存 · ${saved.time}`, options);
  } catch (error) {
    toast(String(error), true);
    return { ok: false, editorChanged: false };
  } finally {
    saving = false;
  }
}

/** 历史条目「续写」：必要时先保存当前草稿，再载入目标并进入续写模式。 */
async function handleContinueEntry(note: NoteEntry): Promise<void> {
  if (isAppLocked()) return;
  if (saving) {
    toast("正在保存，请稍候", true);
    return;
  }
  if (hasOpenEditor()) {
    toast("请先保存或取消正在编辑的笔记", true);
    return;
  }

  const currentRaw = getText();
  const currentTarget = activeContinuation();
  // 当前正文就是某条记录本身（未改动的续写，或退出续写后又点同一条）：
  // 直接丢弃不会丢内容，不写盘，避免生成重复记录。
  const unchangedCurrent = currentTarget !== null && currentRaw === currentTarget.content;
  if (unchangedCurrent || currentRaw === note.content) {
    endContinuation();
  } else if (currentRaw.trim()) {
    const result = await saveNote({ forceAppend: true, hideOnSuccess: false, quiet: true });
    if (!result.ok) return;
    if (result.editorChanged) {
      toast("编辑器里有新输入，未切换到续写", true);
      // 当前内容已经作为新记录落盘，刷新列表让用户看到它
      void refreshNotes();
      return;
    }
  }

  // 预保存是异步的：期间用户可能打开了行内编辑或上锁，不能再切换
  if (isAppLocked()) return;
  if (hasOpenEditor()) {
    toast("请先保存或取消正在编辑的笔记", true);
    return;
  }

  startContinuation(note);
  setText(note.content, { caret: "end" });
  closeHistory(false);
  focusEditor();
  toast(`续写 ${note.time} 的记录`, false, 1600);
}

function openSettings(): void {
  // 历史编辑框还有未保存的改动：开设置会把历史面板关掉、输入丢失。
  if (hasOpenEditor()) {
    toast("请先保存或取消编辑", true);
    return;
  }
  closeHistory(false);
  closeWelcome(false);
  showSettingsPanel();
}

function openHistory(): void {
  closeSettings(false);
  closeWelcome(false);
  showHistoryPanel();
}

async function dismissWelcome(openSettingsAfter = false): Promise<void> {
  // 直接切到设置时不做退出动画，避免两个面板叠在一起
  closeWelcome(!openSettingsAfter);
  if (openSettingsAfter) openSettings();
}

let hydrateStarted = false;

/** 解锁后（或本来就没锁）才加载设置、草稿与同步状态。 */
async function hydrate(): Promise<void> {
  if (hydrateStarted) {
    focusEditor();
    return;
  }
  hydrateStarted = true;
  try {
    await refreshSettings();
    pinned = settings.pinned;
    paintPin();
    paintPreviewButton();
    registerAutoHide();

    await initAutostart();
    await restoreDraft();
    await refreshSyncStatus();

    appReady = true;

    try {
      ui.appVersionLabel.textContent = `v${await invoke<string>("app_version")}`;
    } catch {
      ui.appVersionLabel.textContent = "";
    }

    // 启动后延迟检查，不阻塞窗口显示；失败时静默
    startAutoUpdateChecks();

    // 窗口在启动时就可见（首次运行、快捷键冲突）时立即提示；否则留到第一次唤起
    if (await getCurrentWindow().isVisible()) {
      if (await handleFirstShow()) return;
    }

    focusEditor();
  } catch (error) {
    // 失败后允许下一次解锁重新 hydrate；否则整个进程都不会再加载草稿/设置。
    hydrateStarted = false;
    toast(String(error), true);
  } finally {
    ui.card.classList.remove("booting");
  }
}

async function init(): Promise<void> {
  try {
    const status = await getLockStatus();
    paintLockButton(status);
    if (status.warning) toast(status.warning, true);
    if (status.configured && status.locked) {
      await enterLockedMode(status);
      ui.card.classList.remove("booting");
      return;
    }
    await applyLockStatus(status);
  } catch (error) {
    toast(String(error), true);
    ui.card.classList.remove("booting");
  }
}

async function handleFirstShow(): Promise<boolean> {
  if (await showStartupNotice()) return true;
  if (!settings.onboarded) {
    showWelcomePanel();
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

const captureEditor = initEditor({ mount: ui.editorMount, onChange: scheduleDraftSave });
ui.sourceToggle.hidden = captureEditor.kind !== "codemirror";
initHistory({
  onContinue: (note) => {
    void handleContinueEntry(note).catch((error) => toast(String(error), true));
  },
});
initSettingsPanel();
initLockSettings();
initSync();
initUpdater();
initContinuation();

initAppLock({
  onLocked: () => {
    closeSettings(false);
    closeHistory(false, true);
    closeWelcome(false);
    suspendHistoryForLock();
  },
  onUnlocked: async () => {
    if (!appReady) {
      await hydrate();
    } else if (!anyPanelOpen()) {
      focusEditor();
    }
  },
});

ui.lockButton.addEventListener("click", async () => {
  if (isAppLocked()) return;
  try {
    // 先尽力保存草稿，再让后端上锁；上锁后草稿命令会被拦截。
    await flushDraft();
    await lockNow();
  } catch (error) {
    toast(String(error), true);
  }
});

ui.welcomeStart.addEventListener("click", () => void dismissWelcome());
ui.welcomeSettings.addEventListener("click", () => void dismissWelcome(true));

ui.pinButton.addEventListener("click", async () => {
  const next = !pinned;
  pinned = next;
  paintPin();
  try {
    await invoke("set_pinned", { pinned: next });
    toast(next ? "常驻模式：保存后保持打开" : "保存后自动收起");
  } catch (error) {
    // 后端没落盘就不能让按钮停留在假状态
    if (pinned === next) {
      pinned = !next;
      paintPin();
    }
    toast(String(error), true);
  }
});

ui.settingsButton.addEventListener("click", openSettings);
ui.notesButton.addEventListener("click", openHistory);
ui.closeButton.addEventListener("click", () => void hideWindow());
ui.sourceToggle.addEventListener("click", () => {
  const sourceMode = toggleSourceMode();
  ui.sourceToggle.setAttribute("aria-pressed", String(sourceMode));
  ui.sourceToggle.textContent = sourceMode ? "预览" : "源码";
  ui.sourceToggle.title = sourceMode ? "切换回实时渲染" : "查看 Markdown 源码";
  focusEditor();
});

/**
 * window/document 上的监听在开发热更新与测试里会跨「安装」累积：旧实例仍会
 * 响应同一次按键。统一挂在一个 AbortController 上，测试启动新实例前可以清掉。
 */
const runtimeListeners = new AbortController();
if ((import.meta as { env?: { MODE?: string } }).env?.MODE === "test") {
  (
    globalThis as unknown as { __flashnoteTeardown?: () => void }
  ).__flashnoteTeardown = () => runtimeListeners.abort();
}

window.addEventListener(
  "keydown",
  (event) => {
    // 输入法组合期间不处理任何全局快捷键：Esc 应只取消候选，Ctrl+S 也不该落盘
    if (event.isComposing || event.keyCode === 229) return;

    if (isAppLocked()) {
      trapFocus(event);
      if (event.key === "Escape") event.preventDefault();
      if (
        (event.ctrlKey || event.metaKey) &&
        (event.key === "Enter" || event.key.toLowerCase() === "s")
      ) {
        event.preventDefault();
      }
      // 密码框自己的 Enter 监听仍会收到事件；这里只吞掉会离开锁屏/触发保存的组合。
      return;
    }

    if (isRecording()) {
      handleRecordingKey(event);
      return;
    }

    trapFocus(event);

    // 条目编辑框自己处理 Esc / Ctrl+Enter，避免误关整个面板
    if ((event.target as HTMLElement | null)?.closest(".entry-edit")) return;

    if (event.key === "Escape") {
      event.preventDefault();
      if (!ui.welcomePanel.classList.contains("hidden")) {
        void dismissWelcome();
      } else if (!ui.historyPanel.classList.contains("hidden")) {
        closeHistory();
      } else if (!ui.settingsPanel.classList.contains("hidden")) {
        closeSettings();
      } else {
        void hideWindow();
      }
      return;
    }

    // 面板打开时 Ctrl+S / Ctrl+Enter 属于面板自己：误按会把主输入框（通常为空）
    // 当成保存并收起窗口，用户刚填的同步地址与密码全丢。
    if (anyPanelOpen()) {
      // 面板内这两个组合键归面板自己：既要拦住保存/收起，也要 preventDefault，
      // 否则 WebView 可能弹出「保存网页」。
      if (
        (event.ctrlKey || event.metaKey) &&
        (event.key === "Enter" || event.key.toLowerCase() === "s")
      ) {
        event.preventDefault();
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
  { capture: true, signal: runtimeListeners.signal },
);

/**
 * 预览里的链接绝不能导航主窗口：这个无边框置顶窗口一旦被远程页面接管，
 * 本地 CSP 与 Esc 监听都会失效，只能从托盘退出。统一拦下来交给系统浏览器。
 */
function handleLinkActivation(event: MouseEvent): void {
  const target = event.target;
  if (!(target instanceof Element)) return;
  // 不能只认 HTMLAnchorElement：<area> 与 SVG <a> 都不是它，
  // 漏掉任何一个都会让 WebView 自己导航到远程页面。
  const anchor = target.closest("a[href], area[href]");
  if (!(anchor instanceof Element)) return;

  const href =
    anchor.getAttribute("href") ??
    anchor.getAttributeNS("http://www.w3.org/1999/xlink", "href") ??
    "";
  // 页内锚点保持默认行为，其余一律不让 WebView 自己跳
  if (href.trim().startsWith("#")) return;
  event.preventDefault();

  const url = externalLinkTarget(href);
  if (!url) return;
  void invoke("open_external_url", { url }).catch((error) => toast(String(error), true));
}

document.addEventListener("click", handleLinkActivation, {
  capture: true,
  signal: runtimeListeners.signal,
});
document.addEventListener("auxclick", handleLinkActivation, {
  capture: true,
  signal: runtimeListeners.signal,
});

ui.resizeGrip.addEventListener("pointerdown", (event) => {
  event.preventDefault();
  void getCurrentWindow().startResizeDragging("SouthEast");
});

const reportListenerError = (error: unknown): void => {
  console.error("监听窗口事件失败", error);
};

void listen<ShowPayload>("window-shown", async (event) => {
  // 唤起瞬间可能有一次焦点切换，避免误触发失焦收起
  suppressBlurUntil = Date.now() + 600;
  if (event.payload.locked) {
    try {
      const status = await getLockStatus();
      await enterLockedMode(status);
    } catch (error) {
      toast(String(error), true);
    }
    return;
  }
  if (isAppLocked()) {
    try {
      const status = await getLockStatus();
      // 状态可能仍是锁定（例如托盘刚上锁）：交给 applyLockStatus 分流，
      // 不能无条件进入解锁态。
      await applyLockStatus(status);
    } catch (error) {
      toast(String(error), true);
    }
  }

  setSource(event.payload.source);
  if (!appReady) return;
  if (!ui.historyPanel.classList.contains("hidden")) {
    refreshNotes();
    return;
  }
  if (!ui.settingsPanel.classList.contains("hidden")) return;
  void handleFirstShow()
    .then((handled) => {
      if (!handled) focusEditor();
    })
    .catch((error) => toast(String(error), true));
}).catch(reportListenerError);

void listen("open-settings", () => {
  if (isAppLocked()) return;
  suppressBlurUntil = Date.now() + 600;
  openSettings();
}).catch(reportListenerError);

void listen("open-history", () => {
  if (isAppLocked()) return;
  suppressBlurUntil = Date.now() + 600;
  openHistory();
}).catch(reportListenerError);

void init();
