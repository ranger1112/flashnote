import { check, type Update } from "@tauri-apps/plugin-updater";
import { invoke } from "@tauri-apps/api/core";
import { relaunch } from "@tauri-apps/plugin-process";
import { ui } from "./dom";
import { settings } from "./settings-store";
import { toast } from "./toast";

export function showUpdateBanner(message: string, actionLabel: string | null): void {
  ui.updateBannerText.textContent = message;
  ui.updateBannerAction.hidden = actionLabel === null;
  if (actionLabel) ui.updateBannerAction.textContent = actionLabel;
  ui.updateBanner.classList.remove("hidden");
}

export function hideUpdateBanner(): void {
  ui.updateBanner.classList.add("hidden");
}

/**
 * 设置面板里的更新入口。面板打开时底部的更新横幅会被遮住，
 * 手动检查发现新版本后必须有一个能点的按钮。
 */
function setInlineUpdateAction(label: string | null, disabled = false): void {
  ui.updateNowButton.hidden = label === null;
  ui.updateNowButton.disabled = disabled;
  if (label !== null) ui.updateNowButton.textContent = label;
}

let pendingUpdate: Update | null = null;
let updateReady = false;
let updating = false;
let restarting = false;
/** 检查请求进行中：自动检查与手动检查共用，避免互相把对方的结果作废。 */
let checking = false;
/** 检查请求的单调序号：安装/重启开始后，旧检查结果必须丢弃。 */
let checkSeq = 0;

let autoCheckTimeout: number | undefined;
let autoCheckInterval: number | undefined;

function stopAutoUpdateChecks(): void {
  if (autoCheckTimeout !== undefined) window.clearTimeout(autoCheckTimeout);
  if (autoCheckInterval !== undefined) window.clearInterval(autoCheckInterval);
  autoCheckTimeout = undefined;
  autoCheckInterval = undefined;
}

// 启动 5 秒后检查一次；常驻不关的实例每 6 小时再查一次
export function startAutoUpdateChecks(): void {
  stopAutoUpdateChecks();
  if (!settings.autoUpdate) return;
  autoCheckTimeout = window.setTimeout(() => {
    autoCheckTimeout = undefined;
    void checkForUpdate(false);
    autoCheckInterval = window.setInterval(
      () => void checkForUpdate(false),
      6 * 60 * 60 * 1000,
    );
  }, 5000);
}

async function checkForUpdate(manual: boolean): Promise<void> {
  if (updating || restarting || checking) return;
  // 已下载安装但尚未重启时，再查一次仍会返回同一版本，直接提示重启即可
  if (updateReady) {
    if (manual) {
      ui.updateStatus.textContent = "更新已安装，重启后生效";
      showUpdateBanner("更新已安装，重启后生效", "重启");
      setInlineUpdateAction("重启");
    }
    return;
  }
  checking = true;
  const seq = ++checkSeq;
  if (manual) {
    ui.checkUpdateButton.disabled = true;
    ui.updateStatus.textContent = "正在检查更新…";
  }

  try {
    const update = await check();
    // 请求返回时可能已经开始安装/重启，或又发起了新一轮检查：丢弃这份旧结果，
    // 否则会把「立即更新」写回「重启」按钮，或覆盖安装进度。
    if (seq !== checkSeq || updating || restarting) return;
    if (!update) {
      pendingUpdate = null;
      setInlineUpdateAction(null);
      if (manual) {
        ui.updateStatus.textContent = "已是最新版本";
        toast("已是最新版本");
      }
      return;
    }

    pendingUpdate = update;
    ui.updateStatus.textContent = `发现新版本 ${update.version}`;
    showUpdateBanner(`发现新版本 ${update.version}`, "更新");
    setInlineUpdateAction("立即更新");
  } catch (error) {
    if (seq !== checkSeq || updating || restarting) return;
    // 启动时的静默检查失败不打扰用户；手动检查才提示
    if (manual) {
      ui.updateStatus.textContent = "检查更新失败，请稍后再试";
      toast(String(error), true);
    }
  } finally {
    checking = false;
    if (manual && seq === checkSeq && !updating && !restarting) {
      ui.checkUpdateButton.disabled = false;
    }
  }
}

async function installUpdate(): Promise<void> {
  const update = pendingUpdate;
  if (!update || updating || restarting) return;

  updating = true;
  // 在途的检查请求已经过时，避免它返回后把按钮改回「立即更新」
  checkSeq += 1;
  ui.checkUpdateButton.disabled = true;
  ui.updateBannerAction.disabled = true;
  setInlineUpdateAction("正在更新…", true);
  ui.updateStatus.textContent = "正在下载更新…";
  showUpdateBanner("正在下载更新…", null);

  try {
    let total = 0;
    let received = 0;
    await update.downloadAndInstall((event) => {
      if (event.event === "Started") {
        total = event.data.contentLength ?? 0;
      } else if (event.event === "Progress") {
        received += event.data.chunkLength;
        const percent =
          total > 0 ? Math.min(100, Math.round((received / total) * 100)) : null;
        const message =
          percent === null ? "正在下载更新…" : `正在下载更新… ${percent}%`;
        ui.updateStatus.textContent = message;
        showUpdateBanner(message, null);
      }
    });

    updateReady = true;
    ui.updateStatus.textContent = `已安装 ${update.version}，重启后生效`;
    showUpdateBanner(`新版本 ${update.version} 已安装，重启后生效`, "重启");
    setInlineUpdateAction("重启");
  } catch (error) {
    showUpdateBanner("更新失败，请稍后重试", "重试");
    ui.updateStatus.textContent = "更新失败";
    setInlineUpdateAction("重试");
    toast(String(error), true);
  } finally {
    updating = false;
    ui.checkUpdateButton.disabled = false;
    ui.updateBannerAction.disabled = false;
  }
}

/** 重启前把设置与同步状态落盘，避免更新后重复合并或重复上传。 */
function restartToApplyUpdate(): void {
  if (restarting) return;
  restarting = true;
  checkSeq += 1;
  ui.checkUpdateButton.disabled = true;
  ui.updateNowButton.disabled = true;
  ui.updateBannerAction.disabled = true;
  ui.updateStatus.textContent = "正在重启…";
  if (!ui.updateNowButton.hidden) ui.updateNowButton.textContent = "正在重启…";
  showUpdateBanner("正在重启…", null);

  // 落盘失败也要重启，但必须告诉用户这次「尽力了」。
  void invoke("flush_sync_state")
    .catch(async (error) => {
      console.error("保存同步状态失败", error);
      toast("同步状态未能落盘，仍将重启", true);
      // 留一点时间让提示画出来，否则立刻重启等于没提示。
      await new Promise((resolve) => window.setTimeout(resolve, 1500));
    })
    .finally(() => {
      void relaunch().catch((error) => {
        // relaunch 失败时不能一直停在「正在重启…」：恢复按钮并明确提示。
        restarting = false;
        ui.checkUpdateButton.disabled = false;
        ui.updateStatus.textContent = "重启失败，请手动退出后重新打开";
        setInlineUpdateAction("重启");
        showUpdateBanner("重启失败，请手动退出后重新打开", "重启");
        ui.updateBannerAction.disabled = false;
        toast(String(error), true);
      });
    });
}

/** 模块加载时绑定更新相关按钮。 */
export function initUpdater(): void {
  ui.checkUpdateButton.addEventListener("click", () => {
    void checkForUpdate(true);
  });

  ui.updateNowButton.addEventListener("click", () => {
    if (updating || restarting) return;
    if (updateReady) restartToApplyUpdate();
    else void installUpdate();
  });

  ui.updateBannerAction.addEventListener("click", () => {
    if (restarting) return;
    if (updateReady) restartToApplyUpdate();
    else void installUpdate();
  });

  ui.updateBannerDismiss.addEventListener("click", hideUpdateBanner);
}
