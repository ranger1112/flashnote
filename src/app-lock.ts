import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { applyTheme } from "./appearance";
import { ui } from "./dom";
import { normalizeTheme } from "./lib";
import { setBackgroundInert } from "./panels";

export interface LockStatus {
  configured: boolean;
  locked: boolean;
  remainingSeconds: number | null;
  retryAfterSeconds: number | null;
  unlockMinutes: number;
  theme: string;
  warning: string | null;
}

interface LockHooks {
  /** 进入锁屏前收起所有面板并清掉历史面板的计时器。 */
  onLocked: () => void;
  /** 解锁后执行；首次解锁时负责 hydrate。 */
  onUnlocked: () => Promise<void> | void;
}

let hooks: LockHooks = {
  onLocked: () => {},
  onUnlocked: () => {},
};
let locked = false;
let currentStatus: LockStatus | null = null;
let expiryTimer: number | undefined;
let retryTimer: number | undefined;
let retryRemaining = 0;
let submitting = false;

export function isAppLocked(): boolean {
  return locked;
}

export function currentLockStatus(): LockStatus | null {
  return currentStatus;
}

export function getLockStatus(): Promise<LockStatus> {
  return invoke<LockStatus>("get_lock_status");
}

export function paintLockButton(status: LockStatus): void {
  ui.lockButton.hidden = !(status.configured && !status.locked);
}

export function initAppLock(nextHooks: LockHooks): void {
  hooks = nextHooks;

  ui.lockSubmit.addEventListener("click", () => void submitPassword());
  ui.lockPassword.addEventListener("keydown", (event) => {
    if (event.key !== "Enter" || event.isComposing) return;
    event.preventDefault();
    void submitPassword();
  });

  void listen<LockStatus>("app-locked", async () => {
    try {
      const status = await getLockStatus();
      await applyLockStatus(status);
    } catch (error) {
      console.error("处理应用锁事件失败", error);
    }
  }).catch((error) => console.error("监听应用锁事件失败", error));
}

function clearExpiryTimer(): void {
  if (expiryTimer !== undefined) {
    window.clearTimeout(expiryTimer);
    expiryTimer = undefined;
  }
}

function clearRetryTimer(): void {
  if (retryTimer !== undefined) {
    window.clearInterval(retryTimer);
    retryTimer = undefined;
  }
  retryRemaining = 0;
}

function paintRetry(): void {
  ui.lockPassword.disabled = true;
  ui.lockSubmit.disabled = true;
  ui.lockHint.textContent = `尝试次数过多，请 ${retryRemaining} 秒后重试`;
}

function startRetry(seconds: number | null): void {
  clearRetryTimer();
  retryRemaining = seconds ?? 0;
  if (retryRemaining <= 0) {
    ui.lockPassword.disabled = false;
    ui.lockSubmit.disabled = false;
    ui.lockHint.textContent = "输入密码以继续";
    return;
  }
  paintRetry();
  retryTimer = window.setInterval(() => {
    retryRemaining -= 1;
    if (retryRemaining <= 0) {
      clearRetryTimer();
      ui.lockPassword.disabled = false;
      ui.lockSubmit.disabled = false;
      ui.lockHint.textContent = "输入密码以继续";
      ui.lockError.textContent = "";
      ui.lockPassword.focus();
    } else {
      paintRetry();
    }
  }, 1000);
}

export async function enterLockedMode(status: LockStatus): Promise<void> {
  if (!status.configured || !status.locked) {
    await enterUnlockedMode(status);
    return;
  }
  applyTheme(normalizeTheme(status.theme));
  currentStatus = status;
  locked = true;
  submitting = false;
  clearExpiryTimer();
  hooks.onLocked();

  ui.card.classList.add("locked");
  ui.lockPanel.classList.remove("hidden");
  setBackgroundInert(true);
  ui.undoBar.inert = true;
  ui.lockPassword.value = "";
  ui.lockError.textContent = status.warning ?? "";
  ui.lockHint.textContent = "输入密码以继续";
  paintLockButton(status);
  startRetry(status.retryAfterSeconds);
  window.setTimeout(() => ui.lockPassword.focus(), 0);
}

export async function enterUnlockedMode(status: LockStatus): Promise<void> {
  currentStatus = status;
  const wasLocked = locked;
  clearExpiryTimer();
  clearRetryTimer();
  ui.lockPassword.disabled = false;
  ui.lockSubmit.disabled = false;
  ui.lockError.textContent = "";
  ui.lockHint.textContent = "输入密码以继续";

  // 首次解锁时在遮罩下面完成 hydration，避免正文闪一下。
  await hooks.onUnlocked();

  locked = false;
  ui.lockPassword.value = "";
  ui.lockPanel.classList.add("hidden");
  ui.card.classList.remove("locked");
  if (wasLocked) setBackgroundInert(false);
  paintLockButton(status);
  scheduleExpiry(status);
}

export function scheduleExpiry(status: LockStatus): void {
  clearExpiryTimer();
  if (!status.configured || status.locked || status.remainingSeconds === null) {
    return;
  }
  const delay = Math.min(
    status.remainingSeconds * 1000 + 250,
    2_147_483_000,
  );
  expiryTimer = window.setTimeout(async () => {
    try {
      const next = await getLockStatus();
      if (next.configured && next.locked) {
        await enterLockedMode(next);
      } else {
        scheduleExpiry(next);
      }
    } catch (error) {
      console.error("检查解锁有效期失败", error);
    }
  }, delay);
}

export async function applyLockStatus(status: LockStatus): Promise<void> {
  applyTheme(normalizeTheme(status.theme));
  paintLockButton(status);
  if (status.configured && status.locked) {
    await enterLockedMode(status);
  } else {
    await enterUnlockedMode(status);
  }
}

export async function refreshLockStatus(): Promise<LockStatus> {
  const status = await getLockStatus();
  currentStatus = status;
  paintLockButton(status);
  return status;
}

export async function lockNow(): Promise<void> {
  const status = await invoke<LockStatus>("lock_now");
  await applyLockStatus(status);
}

async function submitPassword(): Promise<void> {
  if (submitting || retryRemaining > 0) return;
  const password = ui.lockPassword.value;
  if (!password) {
    ui.lockError.textContent = "请输入密码";
    return;
  }

  submitting = true;
  ui.lockSubmit.disabled = true;
  ui.lockPassword.disabled = true;
  ui.lockError.textContent = "";
  try {
    const status = await invoke<LockStatus>("unlock", { password });
    ui.lockPassword.value = "";
    await enterUnlockedMode(status);
  } catch (error) {
    ui.lockError.textContent = String(error);
    ui.lockPassword.value = "";
    try {
      const status = await getLockStatus();
      currentStatus = status;
      startRetry(status.retryAfterSeconds);
    } catch {
      // 读取状态失败时保留当前错误；下一次显示/操作会重新查询。
    }
  } finally {
    submitting = false;
    if (retryRemaining <= 0) {
      ui.lockSubmit.disabled = false;
      ui.lockPassword.disabled = false;
      ui.lockPassword.focus();
    }
  }
}
