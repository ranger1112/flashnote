import { invoke } from "@tauri-apps/api/core";
import {
  clampOpacity,
  formatAccel,
  normalizeTheme,
  type ThemeMode,
} from "./lib";
import { applyTheme, paintOpacity } from "./appearance";
import { ui } from "./dom";

export interface Settings {
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

export let settings: Settings = {
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

/** 把当前 settings 回填到设置面板的各个控件。 */
export function paintSettings(): void {
  ui.hotkeyInput.value = formatAccel(settings.hotkey);
  ui.notesDirInput.value = settings.notesDir;
  ui.captureSourceCheck.checked = settings.captureSource;
  ui.markdownModeCheck.checked = settings.markdownMode;
  ui.autoUpdateCheck.checked = settings.autoUpdate;
  ui.autoHideCheck.checked = settings.autoHideOnBlur;
  ui.themeSelect.value = settings.theme;
  paintOpacity(settings.backgroundOpacity);
}

export async function refreshSettings(): Promise<void> {
  settings = await invoke<Settings>("get_settings");
  settings.theme = normalizeTheme(settings.theme);
  settings.backgroundOpacity = clampOpacity(settings.backgroundOpacity);
  applyTheme(settings.theme);
  paintSettings();
}
