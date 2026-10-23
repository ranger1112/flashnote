import { resolveTheme, type ThemeMode } from "./lib";
import { ui } from "./dom";

const systemLight = window.matchMedia("(prefers-color-scheme: light)");
let currentMode: ThemeMode = "system";

export function applyTheme(mode: ThemeMode): void {
  currentMode = mode;
  const resolved = resolveTheme(mode, systemLight.matches);
  document.documentElement.dataset.theme = resolved;
  document.documentElement.style.colorScheme = resolved;
}

export function paintOpacity(value: number): void {
  document.documentElement.style.setProperty("--surface-alpha", String(value));
  ui.bgOpacityInput.value = String(Math.round(value * 100));
  ui.bgOpacityValue.textContent = `${Math.round(value * 100)}%`;
}

systemLight.addEventListener("change", () => {
  if (currentMode === "system") applyTheme("system");
});
