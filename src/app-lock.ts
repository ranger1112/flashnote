import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { applyTheme } from "./appearance";
import { ui } from "./dom";
import { focusEditor } from "./editor";
import { normalizeTheme } from "./lib";
import { anyPanelOpen, setBackgroundInert } from "./panels";

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
/** 每次进入锁屏自增，用来识别 hydration 期间又发生了一次上锁。 */
let lockGeneration = 0;

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

  // 系统时间前跳或休眠恢复后，后端可能已按墙上时钟上锁；回到窗口时补查一次。
  window.addEventListener("focus", () => {
    void getLockStatus()
      .then((status) => {
        if (status.configured && status.locked) return enterLockedMode(status);
        return undefined;
      })
      .catch((error) => console.error("检查应用锁状态失败", error));
  });
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
  lockGeneration += 1;
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
  const generation = lockGeneration;
  clearExpiryTimer();
  clearRetryTimer();
  ui.lockPassword.disabled = false;
  ui.lockSubmit.disabled = false;
  ui.lockError.textContent = "";
  ui.lockHint.textContent = "输入密码以继续";

  // 首次解锁时在遮罩下面完成 hydration，避免正文闪一下。
  await hooks.onUnlocked();

  // hydration 期间可能又上锁了：不能把新的锁屏覆盖掉。
  // 注意不能用 `locked` 判断——进入本函数时它本来就已经是 true。
  if (generation !== lockGeneration) return;

  locked = false;
  ui.lockPassword.value = "";
  ui.lockPanel.classList.add("hidden");
  ui.card.classList.remove("locked");
  if (wasLocked) setBackgroundInert(false);
  // 锁屏关闭时如果也没有其他面板，顺手清掉 panel-open，并把焦点还给输入框。
  if (!anyPanelOpen()) {
    ui.card.classList.remove("panel-open");
    focusEditor();
  }
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
      // 查询失败不能一直保持解锁：稍后重试，仍失败就按已锁定处理。
      expiryTimer = window.setTimeout(async () => {
        try {
          const next = await getLockStatus();
          if (next.configured && next.locked) {
            await enterLockedMode(next);
          } else {
            scheduleExpiry(next);
          }
        } catch (retryError) {
          console.error("检查解锁有效期失败", retryError);
          if (currentStatus?.configured) {
            await enterLockedMode({
              ...currentStatus,
              locked: true,
              remainingSeconds: null,
            });
          }
        }
      }, 5_000);
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
    // 解锁成功后锁屏已经收起，不要再把焦点抢到隐藏的密码框上。
    if (retryRemaining <= 0 && isAppLocked()) {
      ui.lockSubmit.disabled = false;
      ui.lockPassword.disabled = false;
      ui.lockPassword.focus();
    }
  }
}
