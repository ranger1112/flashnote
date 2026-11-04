import { invoke } from "@tauri-apps/api/core";
import { isAppLocked } from "./app-lock";
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
  // 上锁导致的关闭不算完成引导，也不能调用被锁拦截的命令。
  if (!isAppLocked()) {
    settings.onboarded = true;
    void invoke("complete_onboarding").catch(() => {
      // 下次启动时重新展示引导
    });
  }
  hidePanel(ui.welcomePanel, animate);
}
