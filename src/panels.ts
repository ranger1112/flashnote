import { ui } from "./dom";

export const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");
// 与 styles.css 里的动画时长保持一致
export const PANEL_IN_MS = 160;
export const PANEL_OUT_MS = 120;

/**
 * 面板打开时让背景不可聚焦，关闭后恢复。
 * #toast 不参与：撤销删除的按钮要能在面板打开时点击。
 */
export function setBackgroundInert(inert: boolean): void {
  for (const selector of ["header", "main", "footer", "#resize-grip", "#update-banner"]) {
    const node = document.querySelector<HTMLElement>(selector);
    if (node) node.inert = inert;
  }
}

export function anyPanelOpen(): boolean {
  return [ui.welcomePanel, ui.settingsPanel, ui.historyPanel].some(
    (panel) => !panel.classList.contains("hidden"),
  );
}

/** 把 Tab 焦点限制在当前打开的面板内。 */
export function trapFocus(event: KeyboardEvent): void {
  if (event.key !== "Tab") return;
  const panel = [ui.welcomePanel, ui.settingsPanel, ui.historyPanel].find(
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
export function showPanel(panel: HTMLElement): void {
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
  ui.card.classList.add("panel-open");
  setBackgroundInert(true);
}

/** 关闭面板：动画结束后才真正 hidden，焦点与 inert 也在那时恢复。 */
export function hidePanel(panel: HTMLElement, animate = true): void {
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
export function afterPanelClosed(): void {
  if (anyPanelOpen()) return;
  ui.card.classList.remove("panel-open");
  setBackgroundInert(false);
  ui.textarea.focus();
}
