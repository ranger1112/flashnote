import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ui } from "./dom";
import { refreshSettings, settings } from "./settings-store";
import { toast } from "./toast";

/**
 * 云同步 UI：设置面板里的配置表单、历史面板底部的状态与冲突提示。
 *
 * 状态由后端驱动：每轮同步结束会广播 `sync-status`，这里只负责把它翻译成
 * 一行中文；表单保存成功后才真正落盘，失败时保留用户输入。
 */

export interface SyncSummary {
  uploaded: number;
  downloaded: number;
  merged: number;
  deleted: number;
  skipped: number;
  conflicts: number;
  aborted: boolean;
  errors: string[];
  warnings: string[];
  startedAt: string;
  finishedAt: string | null;
}

export interface SyncStatus {
  configured: boolean;
  enabled: boolean;
  running: boolean;
  lastSyncAt: string | null;
  lastError: string | null;
  degraded: string | null;
  summary: SyncSummary | null;
  conflicts: number;
}

interface SyncTestResult {
  files: number;
  degraded: string | null;
  message: string;
}

interface PasswordSaveResult {
  persistent: boolean;
  warning: string | null;
}

function formatTime(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return date.toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" });
}

let pendingSince: number | null = null;
let lastStatus: SyncStatus | null = null;
/// 状态刷新是异步的：慢响应回来时如果已经有更新的状态，直接丢弃。
let statusSeq = 0;
let savingConfig = false;

/** 本地写入成功后调用：下一轮同步开始前显示「待同步」。 */
export function markSyncPending(): void {
  if (!settings.sync.enabled) return;
  pendingSince = Date.now();
  if (lastStatus) paintSyncIndicator(lastStatus);
}

function paintSyncIndicator(status: SyncStatus): void {
  const el = ui.syncIndicator;
  el.className = "sync-indicator";
  el.removeAttribute("title");
  if (!status.configured || !status.enabled) {
    el.textContent = "";
    pendingSince = null;
    return;
  }
  if (status.running) {
    el.textContent = "同步中";
    el.classList.add("running");
    return;
  }
  if (status.lastError) {
    el.textContent = "同步失败";
    el.classList.add("error");
    el.title = status.lastError;
    return;
  }
  if (pendingSince !== null) {
    const started = status.summary?.startedAt ? Date.parse(status.summary.startedAt) : NaN;
    if (!Number.isNaN(started) && started >= pendingSince) {
      pendingSince = null;
    }
  }
  if (pendingSince !== null) {
    el.textContent = "待同步";
    el.classList.add("pending");
    return;
  }
  if (status.lastSyncAt) {
    el.textContent = "已同步";
    el.classList.add("synced");
    el.title = `上次同步 ${formatTime(status.lastSyncAt)}`;
  } else {
    el.textContent = "等待同步";
  }
}

/** 一句话状态：设置区显示完整版，历史面板只显示第一段。 */
export function describeStatus(status: SyncStatus): string[] {
  const parts: string[] = [];
  if (!status.configured) {
    parts.push("未配置");
  } else if (!status.enabled) {
    parts.push("已关闭");
  } else if (status.running) {
    parts.push("正在同步…");
  } else if (status.lastError) {
    parts.push(`同步失败：${status.lastError}`);
  } else if (status.lastSyncAt) {
    parts.push(`上次同步 ${formatTime(status.lastSyncAt)}`);
  } else {
    parts.push("等待首次同步");
  }

  if (status.degraded) parts.push(status.degraded);

  // 只有启用中的配置才展示上一轮明细，否则「未配置」旁边挂一串数字很误导
  const summary = status.summary;
  if (summary && status.configured && status.enabled && !status.running && !status.lastError) {
    const changed =
      summary.uploaded + summary.downloaded + summary.merged + summary.deleted;
    if (changed > 0) {
      parts.push(
        `上传 ${summary.uploaded} · 下载 ${summary.downloaded} · 合并 ${summary.merged} · 删除 ${summary.deleted}`,
      );
    }
  }
  return parts;
}

export function paintSyncStatus(status: SyncStatus): void {
  const previous = lastStatus;
  lastStatus = status;
  // 有任何更新来源（保存、广播、手动刷新）都作废在途的旧响应。
  statusSeq += 1;
  paintSyncIndicator(status);
  // 只有「又成功同步了一轮」才清掉红色错误行：单纯刷新状态不能抹掉
  // 「地址已变更，请重新填写密码」这类还需要用户处理的提示。
  if (!status.lastError && status.lastSyncAt !== previous?.lastSyncAt) {
    ui.syncError.textContent = "";
  }

  ui.syncStatusText.textContent = describeStatus(status).join(" · ");
  ui.syncStatusText.classList.toggle("warn", Boolean(status.degraded));
  ui.syncStatusText.classList.toggle("bad", Boolean(status.lastError));

  ui.syncFootStatus.textContent =
    status.configured && status.enabled ? describeStatus(status)[0] : "云同步未启用";
  ui.syncConflicts.hidden = status.conflicts === 0;
  ui.syncConflicts.textContent = `${status.conflicts} 个冲突副本`;
}

/** 用已保存的设置回填表单；密码框始终留空，避免把密码画到界面上。 */
export function paintSyncForm(): void {
  const sync = settings.sync;
  ui.syncEnabled.checked = sync.enabled;
  ui.syncUrl.value = sync.url;
  ui.syncUsername.value = sync.username;
  ui.syncPassword.value = "";
  ui.syncInterval.value = String(sync.intervalMinutes);
  ui.syncAllowHttp.checked = sync.allowInsecureHttp;
  ui.syncDevice.value = sync.deviceName;
  ui.syncError.textContent = "";
}

function readSyncForm() {
  return {
    enabled: ui.syncEnabled.checked,
    url: ui.syncUrl.value.trim(),
    username: ui.syncUsername.value.trim(),
    intervalMinutes: Number(ui.syncInterval.value),
    allowInsecureHttp: ui.syncAllowHttp.checked,
    deviceName: ui.syncDevice.value.trim(),
  };
}

/** 只差末尾斜杠的地址是同一个远端。 */
/** 与后端 `remote_identity` 同口径：域名大小写、query、尾斜杠都不算换远端。 */
function normalizeSyncUrl(value: string): string {
  const trimmed = value.trim();
  try {
    const url = new URL(trimmed);
    url.search = "";
    url.hash = "";
    return url.toString().replace(/\/+$/, "");
  } catch {
    return trimmed.replace(/\/+$/, "");
  }
}

/** 公网明文 http 需要用户再确认一次；本机与私网地址不打扰。 */
function needsInsecureConfirmation(url: string): boolean {
  try {
    const parsed = new URL(url);
    if (parsed.protocol !== "http:") {
      return false;
    }
    const host = parsed.hostname;
    const isLoopback = host === "localhost" || host === "127.0.0.1" || host === "[::1]";
    const isPrivate =
      /^10\./.test(host) ||
      /^192\.168\./.test(host) ||
      /^172\.(1[6-9]|2\d|3[01])\./.test(host);
    return !isLoopback && !isPrivate;
  } catch {
    return false;
  }
}

async function saveSyncConfig(): Promise<void> {
  if (savingConfig) {
    return;
  }
  ui.syncError.textContent = "";
  const form = readSyncForm();
  const identityChanged =
    normalizeSyncUrl(settings.sync.url) !== normalizeSyncUrl(form.url) ||
    settings.sync.username.trim() !== form.username;
  if (
    form.allowInsecureHttp &&
    needsInsecureConfirmation(form.url) &&
    typeof window.confirm === "function" &&
    !window.confirm("这个地址不是本机或内网，笔记与密码会以明文传输。仍要保存吗？")
  ) {
    return;
  }

  savingConfig = true;
  ui.syncSave.disabled = true;
  try {
    const status = await invoke<SyncStatus>("update_sync_config", {
      config: form,
    });
    // 配置先落盘，再单独保存密码：凭据管理器失败不该回滚服务器地址。
    const password = ui.syncPassword.value;
    let passwordFailed = false;
    if (password) {
      try {
        const saved = await invoke<PasswordSaveResult>("save_sync_password", { password });
        ui.syncPassword.value = "";
        if (saved.warning) toast(saved.warning, true, 6000);
        // 凭据管理器不可用时后端仍返回成功（密码只在本次会话内存里），
        // 这里必须按失败提示，否则用户以为重启后还能同步。
        if (!saved.persistent) {
          passwordFailed = true;
        }
      } catch (error) {
        passwordFailed = true;
        toast(String(error), true);
      }
    }
    await refreshSettings();
    paintSyncStatus(status);
    if (passwordFailed) {
      // 绝不能用「已保存」盖掉刚刚的密码错误：用户会以为下次启动还能同步。
      const message = "配置已保存，但密码未能保存；请重试或检查系统凭据管理器";
      ui.syncError.textContent = message;
      toast(message, true, 6000);
    } else {
      toast("云同步设置已保存");
    }
    if (identityChanged && !password) {
      // 后端已清掉旧凭据，这里必须明确告诉用户，否则下一轮只会看到“同步失败”。
      const message = "地址或用户名已变更，请重新填写密码";
      ui.syncError.textContent = message;
      toast(message, true, 6000);
    }
  } catch (error) {
    ui.syncError.textContent = String(error);
    toast(String(error), true);
  } finally {
    savingConfig = false;
    ui.syncSave.disabled = false;
  }
}

async function testConnection(): Promise<void> {
  ui.syncError.textContent = "";
  ui.syncTest.disabled = true;
  try {
    const password = ui.syncPassword.value || null;
    const result = await invoke<SyncTestResult>("test_sync_connection", {
      config: readSyncForm(),
      password,
    });
    ui.syncStatusText.textContent = result.message;
    ui.syncStatusText.classList.remove("bad");
    ui.syncStatusText.classList.toggle("warn", Boolean(result.degraded));
    toast(result.message);
  } catch (error) {
    ui.syncError.textContent = String(error);
    toast(String(error), true);
    // 失败时状态行可能还停在「已连接」，直接改成失败，避免三处信息互相矛盾。
    ui.syncStatusText.textContent = "测试连接失败";
    ui.syncStatusText.classList.remove("warn");
    ui.syncStatusText.classList.add("bad");
  } finally {
    ui.syncTest.disabled = false;
  }
}

async function syncNow(): Promise<void> {
  try {
    await invoke("sync_now");
    toast("已开始同步");
  } catch (error) {
    toast(String(error), true);
  }
}

export async function refreshSyncStatus(): Promise<void> {
  const seq = ++statusSeq;
  try {
    const status = await invoke<SyncStatus>("get_sync_status");
    if (seq !== statusSeq) {
      return; // 期间已经有更新的状态，丢弃这份旧快照
    }
    paintSyncStatus(status);
  } catch (error) {
    // 状态读不到不影响主流程，下一次广播会补上
    console.error("读取同步状态失败", error);
  }
}

/** 模块加载时绑定同步相关控件与事件（由 main.ts 调用一次）。 */
export function initSync(): void {
  ui.syncSave.addEventListener("click", () => void saveSyncConfig());
  ui.syncTest.addEventListener("click", () => void testConnection());
  ui.syncNow.addEventListener("click", () => void syncNow());
  ui.syncConflicts.addEventListener("click", () => {
    void invoke("open_notes_dir").catch((error) => toast(String(error), true));
  });

  void listen<SyncStatus>("sync-status", (event) => paintSyncStatus(event.payload)).catch(
    (error) => console.error("监听同步状态失败", error),
  );
  void refreshSyncStatus();
}
