import { invoke } from "@tauri-apps/api/core";
import { paintLockButton, type LockStatus } from "./app-lock";
import { ui } from "./dom";
import { toast } from "./toast";

const LABELS: Record<number, string> = {
  30: "30 分钟",
  120: "2 小时",
  240: "4 小时",
  480: "8 小时",
  1440: "24 小时",
};

let current: LockStatus | null = null;

function label(minutes: number): string {
  return LABELS[minutes] ?? `${minutes} 分钟`;
}

function hideForms(): void {
  ui.lockChangeForm.classList.add("hidden");
  ui.lockDisableForm.classList.add("hidden");
  ui.lockManageError.textContent = "";
  ui.lockDisableError.textContent = "";
}

function paint(status: LockStatus): void {
  current = status;
  ui.lockSettingsStatus.textContent = status.configured
    ? `已启用 · 解锁后 ${label(status.unlockMinutes)} 有效`
    : "未启用";
  ui.lockSettingsWarning.textContent = status.warning ?? "";
  ui.lockSetup.classList.toggle("hidden", status.configured);
  ui.lockManage.classList.toggle("hidden", !status.configured);
  ui.lockValidity.value = String(status.unlockMinutes);
  hideForms();
  ui.lockSetupError.textContent = "";
  paintLockButton(status);
}

export async function refreshLockSettings(): Promise<void> {
  const status = await invoke<LockStatus>("get_lock_status");
  paint(status);
}

function checkNewPassword(password: string, confirm: string): string | null {
  if (password.length < 8) return "密码至少需要 8 个字符";
  if (password !== confirm) return "两次输入的密码不一致";
  return null;
}

async function enable(): Promise<void> {
  const password = ui.lockNewPassword.value;
  const confirm = ui.lockConfirmPassword.value;
  const problem = checkNewPassword(password, confirm);
  if (problem) {
    ui.lockSetupError.textContent = problem;
    return;
  }
  ui.lockSetupError.textContent = "";
  ui.lockEnable.disabled = true;
  try {
    const status = await invoke<LockStatus>("set_app_lock_password", {
      currentPassword: null,
      newPassword: password,
      unlockMinutes: Number(ui.lockValidity.value),
    });
    ui.lockNewPassword.value = "";
    ui.lockConfirmPassword.value = "";
    paint(status);
    toast("应用锁已启用");
  } catch (error) {
    ui.lockSetupError.textContent = String(error);
  } finally {
    ui.lockEnable.disabled = false;
  }
}

async function change(): Promise<void> {
  const currentPassword = ui.lockCurrentPassword.value;
  const password = ui.lockChangePassword.value;
  const confirm = ui.lockChangeConfirm.value;
  if (!currentPassword) {
    ui.lockManageError.textContent = "请输入当前密码";
    return;
  }
  const problem = checkNewPassword(password, confirm);
  if (problem) {
    ui.lockManageError.textContent = problem;
    return;
  }
  ui.lockManageError.textContent = "";
  ui.lockChangeSave.disabled = true;
  try {
    const status = await invoke<LockStatus>("set_app_lock_password", {
      currentPassword,
      newPassword: password,
      unlockMinutes: Number(ui.lockValidity.value),
    });
    ui.lockCurrentPassword.value = "";
    ui.lockChangePassword.value = "";
    ui.lockChangeConfirm.value = "";
    paint(status);
    toast("应用锁密码已更新");
  } catch (error) {
    ui.lockManageError.textContent = String(error);
  } finally {
    ui.lockChangeSave.disabled = false;
  }
}

async function disable(): Promise<void> {
  const password = ui.lockDisablePassword.value;
  if (!password) {
    ui.lockDisableError.textContent = "请输入当前密码";
    return;
  }
  ui.lockDisableError.textContent = "";
  ui.lockDisableSave.disabled = true;
  try {
    const status = await invoke<LockStatus>("disable_app_lock", { password });
    ui.lockDisablePassword.value = "";
    paint(status);
    toast("应用锁已关闭");
  } catch (error) {
    ui.lockDisableError.textContent = String(error);
  } finally {
    ui.lockDisableSave.disabled = false;
  }
}

async function saveValidity(): Promise<void> {
  const minutes = Number(ui.lockValidity.value);
  try {
    const status = await invoke<LockStatus>("update_lock_validity", { minutes });
    paint(status);
    toast(`解锁有效期已设为 ${label(status.unlockMinutes)}`);
  } catch (error) {
    ui.lockValidity.value = String(current?.unlockMinutes ?? 30);
    ui.lockSettingsWarning.textContent = String(error);
  }
}

export function initLockSettings(): void {
  ui.lockEnable.addEventListener("click", () => void enable());
  ui.lockChange.addEventListener("click", () => {
    hideForms();
    ui.lockChangeForm.classList.remove("hidden");
    ui.lockCurrentPassword.focus();
  });
  ui.lockChangeCancel.addEventListener("click", hideForms);
  ui.lockChangeSave.addEventListener("click", () => void change());
  ui.lockDisable.addEventListener("click", () => {
    hideForms();
    ui.lockDisableForm.classList.remove("hidden");
    ui.lockDisablePassword.focus();
  });
  ui.lockDisableCancel.addEventListener("click", hideForms);
  ui.lockDisableSave.addEventListener("click", () => void disable());
  ui.lockValidity.addEventListener("change", () => void saveValidity());
}
