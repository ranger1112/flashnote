import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { externalLinkTarget } from "./lib";
import { ui } from "./dom";
import { toast } from "./toast";
import { anyPanelOpen, setBackgroundInert, trapFocus } from "./panels";
import { refreshSettings, settings } from "./settings-store";
import {
  clearStoredDraft,
  flushDraft,
  initDraft,
  restoreDraft,
  scheduleDraftSave,
} from "./draft";
import { initSync, markSyncPending } from "./sync";
import { initUpdater, startAutoUpdateChecks } from "./updater";
import {
  closeHistory,
  hasOpenEditor,
  initHistory,
  paintPreviewButton,
  refreshNotes,
  showHistoryPanel,
} from "./history";
import {
  closeSettings,
  handleRecordingKey,
  initAutostart,
  initSettingsPanel,
  isRecording,
  showSettingsPanel,
} from "./settings-panel";
import { closeWelcome, showWelcomePanel } from "./welcome";

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
  // 收起窗口时不做退出动画，避免面板还停留在一半
  closeSettings(false);
  // 收起窗口是明确的「离开」动作：编辑框内容随窗口一起收起，不阻塞热键。
  closeHistory(false, true);
  closeWelcome(false);
  setBackgroundInert(false);
  await flushDraft();
  await invoke("hide_window");
}

async function saveNote(): Promise<void> {
  if (saving) return;

  const content = ui.textarea.value.trim();
  if (!content) {
    await hideWindow();
    return;
  }

  saving = true;
  try {
    const saved = await invoke<SavedNote>("save_note", { content });
    // 已经在磁盘上产生变更，先标记待同步；真正的同步结果由 sync-status 广播回来。
    markSyncPending();
    // 保存期间用户可能继续输入：只有内容没变时才清空，否则保留新内容并补一次草稿
    if (ui.textarea.value.trim() === content) {
      ui.textarea.value = "";
      clearStoredDraft();
      toast(`已保存 · ${saved.time}`);
      if (!pinned) {
        window.setTimeout(() => {
          void invoke("hide_window");
        }, 380);
      }
    } else {
      scheduleDraftSave();
      toast(`已保存 · ${saved.time}（新输入尚未保存）`);
    }
  } catch (error) {
    toast(String(error), true);
  } finally {
    saving = false;
  }
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

async function init(): Promise<void> {
  try {
    await refreshSettings();
    pinned = settings.pinned;
    paintPin();
    paintPreviewButton();
    registerAutoHide();

    await initAutostart();

    await restoreDraft();

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

    ui.textarea.focus();
  } catch (error) {
    toast(String(error), true);
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

initDraft();
initHistory();
initSettingsPanel();
initSync();
initUpdater();

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
  const anchor = target.closest("a[href]");
  if (!(anchor instanceof HTMLAnchorElement)) return;

  const href = anchor.getAttribute("href") ?? "";
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

void listen<ShowPayload>("window-shown", (event) => {
  // 唤起瞬间可能有一次焦点切换，避免误触发失焦收起
  suppressBlurUntil = Date.now() + 600;
  setSource(event.payload.source);
  if (!appReady) return;
  if (!ui.historyPanel.classList.contains("hidden")) {
    refreshNotes();
    return;
  }
  if (!ui.settingsPanel.classList.contains("hidden")) return;
  void handleFirstShow()
    .then((handled) => {
      if (!handled) ui.textarea.focus();
    })
    .catch((error) => toast(String(error), true));
}).catch(reportListenerError);

void listen("open-settings", () => {
  suppressBlurUntil = Date.now() + 600;
  openSettings();
}).catch(reportListenerError);

void listen("open-history", () => {
  suppressBlurUntil = Date.now() + 600;
  openHistory();
}).catch(reportListenerError);

void init();
