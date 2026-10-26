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

let pendingUpdate: Update | null = null;
let updateReady = false;
let updating = false;

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
  if (updating) return;
  // 已下载安装但尚未重启时，再查一次仍会返回同一版本，直接提示重启即可
  if (updateReady) {
    if (manual) {
      ui.updateStatus.textContent = "更新已安装，重启后生效";
      showUpdateBanner("更新已安装，重启后生效", "重启");
    }
    return;
  }
  if (manual) {
    ui.checkUpdateButton.disabled = true;
    ui.updateStatus.textContent = "正在检查更新…";
  }

  try {
    const update = await check();
    if (!update) {
      if (manual) {
        ui.updateStatus.textContent = "已是最新版本";
        toast("已是最新版本");
      }
      return;
    }

    pendingUpdate = update;
    ui.updateStatus.textContent = `发现新版本 ${update.version}`;
    showUpdateBanner(`发现新版本 ${update.version}`, "更新");
  } catch (error) {
    // 启动时的静默检查失败不打扰用户；手动检查才提示
    if (manual) {
      ui.updateStatus.textContent = "检查更新失败，请稍后再试";
      toast(String(error), true);
    }
  } finally {
    if (manual) ui.checkUpdateButton.disabled = false;
  }
}

async function installUpdate(): Promise<void> {
  const update = pendingUpdate;
  if (!update || updating) return;

  updating = true;
  ui.updateBannerAction.disabled = true;
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
        showUpdateBanner(
          percent === null ? "正在下载更新…" : `正在下载更新… ${percent}%`,
          null,
        );
      }
    });

    updateReady = true;
    ui.updateStatus.textContent = `已安装 ${update.version}，重启后生效`;
    showUpdateBanner(`新版本 ${update.version} 已安装，重启后生效`, "重启");
  } catch (error) {
    showUpdateBanner("更新失败，请稍后重试", "重试");
    ui.updateStatus.textContent = "更新失败";
    toast(String(error), true);
  } finally {
    updating = false;
    ui.updateBannerAction.disabled = false;
  }
}

/** 模块加载时绑定更新相关按钮。 */
export function initUpdater(): void {
  ui.checkUpdateButton.addEventListener("click", () => {
    void checkForUpdate(true);
  });

  ui.updateBannerAction.addEventListener("click", () => {
    if (updateReady) {
      // 重启前把设置与同步状态落盘，避免更新后重复合并或重复上传。
      // 落盘失败也要重启，但必须告诉用户这次「尽力了」。
      void invoke("flush_sync_state")
        .catch((error) => {
          console.error("保存同步状态失败", error);
          toast("同步状态未能落盘，仍将重启", true);
        })
        .finally(() => void relaunch());
    } else {
      void installUpdate();
    }
  });

  ui.updateBannerDismiss.addEventListener("click", hideUpdateBanner);
}
