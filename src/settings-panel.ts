import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { disable, enable, isEnabled } from "@tauri-apps/plugin-autostart";
import {
  accelFromEvent,
  clampOpacity,
  normalizeTheme,
} from "./lib";
import { ui } from "./dom";
import { paintSettings, refreshSettings, settings } from "./settings-store";
import { applyTheme, paintOpacity } from "./appearance";
import { hidePanel, showPanel } from "./panels";
import { toast } from "./toast";
import { paintSyncForm, refreshSyncStatus } from "./sync";
import { startAutoUpdateChecks } from "./updater";

let recording = false;

export function isRecording(): boolean {
  return recording;
}

function stopRecording(): void {
  recording = false;
  ui.hotkeyRecord.classList.remove("recording");
  ui.hotkeyRecord.textContent = "录制";
}

function startRecording(): void {
  recording = true;
  ui.hotkeyRecord.classList.add("recording");
  ui.hotkeyRecord.textContent = "按键…";
  ui.hotkeyError.textContent = "";
}

async function applyHotkey(accel: string): Promise<void> {
  try {
    await invoke("update_hotkey", { accel });
    await refreshSettings();
    stopRecording();
    toast("快捷键已更新");
  } catch (error) {
    ui.hotkeyError.textContent = String(error);
    stopRecording();
  }
}

export function handleRecordingKey(event: KeyboardEvent): void {
  event.preventDefault();
  event.stopPropagation();

  if (event.key === "Escape") {
    stopRecording();
    return;
  }

  const accel = accelFromEvent(event);
  if (accel) void applyHotkey(accel);
}

export function showSettingsPanel(): void {
  paintSettings();
  paintSyncForm();
  void refreshSyncStatus();
  ui.hotkeyError.textContent = "";
  showPanel(ui.settingsPanel);
  ui.settingsClose.focus();
}

export function closeSettings(animate = true): void {
  if (ui.settingsPanel.classList.contains("hidden")) return;
  stopRecording();
  hidePanel(ui.settingsPanel, animate);
}

/** 启动时读取开机自启状态；平台不支持时禁用开关。 */
export async function initAutostart(): Promise<void> {
  try {
    ui.autostartCheck.checked = await isEnabled();
  } catch {
    ui.autostartCheck.disabled = true;
  }
}

/** 模块加载时绑定设置面板内部控件。 */
export function initSettingsPanel(): void {
  ui.settingsClose.addEventListener("click", () => closeSettings());

  ui.hotkeyRecord.addEventListener("click", () => {
    if (recording) {
      stopRecording();
    } else {
      startRecording();
    }
  });

  ui.chooseDirButton.addEventListener("click", async () => {
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

  ui.captureSourceCheck.addEventListener("change", async () => {
    try {
      await invoke("update_capture_source", {
        enabled: ui.captureSourceCheck.checked,
      });
      await refreshSettings();
    } catch (error) {
      ui.captureSourceCheck.checked = !ui.captureSourceCheck.checked;
      toast(String(error), true);
    }
  });

  ui.markdownModeCheck.addEventListener("change", async () => {
    try {
      await invoke("update_markdown_mode", {
        enabled: ui.markdownModeCheck.checked,
      });
      await refreshSettings();
      toast(ui.markdownModeCheck.checked ? "已开启 Markdown 模式" : "已关闭 Markdown 模式");
    } catch (error) {
      ui.markdownModeCheck.checked = !ui.markdownModeCheck.checked;
      toast(String(error), true);
    }
  });

  ui.autoUpdateCheck.addEventListener("change", async () => {
    try {
      await invoke("update_auto_update", { enabled: ui.autoUpdateCheck.checked });
      await refreshSettings();
      startAutoUpdateChecks();
      toast(ui.autoUpdateCheck.checked ? "已开启自动检查更新" : "已关闭自动检查更新");
    } catch (error) {
      ui.autoUpdateCheck.checked = !ui.autoUpdateCheck.checked;
      toast(String(error), true);
    }
  });

  ui.autoHideCheck.addEventListener("change", async () => {
    try {
      await invoke("update_auto_hide", { enabled: ui.autoHideCheck.checked });
      settings.autoHideOnBlur = ui.autoHideCheck.checked;
      toast(ui.autoHideCheck.checked ? "已开启失焦收起" : "已关闭失焦收起");
    } catch (error) {
      ui.autoHideCheck.checked = !ui.autoHideCheck.checked;
      toast(String(error), true);
    }
  });

  ui.themeSelect.addEventListener("change", async () => {
    const previous = settings.theme;
    const next = normalizeTheme(ui.themeSelect.value);
    settings.theme = next;
    applyTheme(next);
    try {
      settings.theme = normalizeTheme(await invoke<string>("update_theme", { theme: next }));
      ui.themeSelect.value = settings.theme;
      applyTheme(settings.theme);
      toast("主题已更新");
    } catch (error) {
      settings.theme = previous;
      ui.themeSelect.value = previous;
      applyTheme(previous);
      toast(String(error), true);
    }
  });

  ui.bgOpacityInput.addEventListener("input", () => {
    paintOpacity(clampOpacity(Number(ui.bgOpacityInput.value) / 100));
  });

  ui.bgOpacityInput.addEventListener("change", async () => {
    const previous = settings.backgroundOpacity;
    const value = clampOpacity(Number(ui.bgOpacityInput.value) / 100);
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

  ui.autostartCheck.addEventListener("change", async () => {
    try {
      if (ui.autostartCheck.checked) {
        await enable();
      } else {
        await disable();
      }
      toast(ui.autostartCheck.checked ? "已开启开机自启" : "已关闭开机自启");
    } catch (error) {
      ui.autostartCheck.checked = !ui.autostartCheck.checked;
      toast(String(error), true);
    }
  });
}
