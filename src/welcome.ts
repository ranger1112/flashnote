import { invoke } from "@tauri-apps/api/core";
import { formatAccel } from "./lib";
import { ui } from "./dom";
import { settings } from "./settings-store";
import { hidePanel, showPanel } from "./panels";

export function showWelcomePanel(): void {
  ui.welcomeHotkey.textContent = formatAccel(settings.hotkey);
  ui.welcomeDir.textContent = settings.notesDir;
  showPanel(ui.welcomePanel);
  ui.welcomeStart.focus();
}

export function closeWelcome(animate = true): void {
  if (ui.welcomePanel.classList.contains("hidden")) return;
  settings.onboarded = true;
  void invoke("complete_onboarding").catch(() => {
    // 下次启动时重新展示引导
  });
  hidePanel(ui.welcomePanel, animate);
}
