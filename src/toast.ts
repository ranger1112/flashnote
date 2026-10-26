import { ui } from "./dom";

let toastTimer: number | undefined;

export function hideToast(): void {
  if (toastTimer !== undefined) {
    window.clearTimeout(toastTimer);
    toastTimer = undefined;
  }
  ui.toastEl.classList.remove("show");
}

export function toast(message: string, error = false, duration = 1600): void {
  ui.toastEl.setAttribute("role", error ? "alert" : "status");
  ui.toastEl.setAttribute("aria-live", error ? "assertive" : "polite");
  ui.toastMessage.textContent = message;
  ui.toastEl.classList.toggle("error", error);
  ui.toastEl.classList.add("show");
  if (toastTimer !== undefined) window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => ui.toastEl.classList.remove("show"), duration);
}

/** 撤销条独立于普通提示，删除待确认期间一直可见，不会被其他 toast 顶掉。 */
export function showUndoBar(message: string): void {
  ui.undoMessage.textContent = message;
  ui.undoBar.removeAttribute("inert");
  ui.undoBar.classList.add("show");
}

export function hideUndoBar(): void {
  ui.undoBar.classList.remove("show");
  // 只改透明度的话，“撤销”按钮仍会进入 Tab 序，读屏也会播报一个看不见的按钮。
  ui.undoBar.setAttribute("inert", "");
}
