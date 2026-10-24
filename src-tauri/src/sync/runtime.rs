//! 同步运行时：配置快照、单飞、调度与状态广播。
//!
//! `SyncEngine` 只负责一轮的具体动作；这里决定「什么时候跑」，并把结果变成
//! 前端可见的 `SyncStatus`。调度规则见计划：保存后防抖 5 秒、启动 60 秒后首轮、
//! 之后按 5/15/60 分钟轮询，手动触发立即执行。
//!
//! 并发约定：同一时刻只有一轮在跑；跑的过程中来的触发只记一个「待办」，
//! 当前轮结束后再补一轮，绝不叠加。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Notify;

use super::backend::{Capabilities, SyncBackend};
use super::secrets;
use super::state::{now_iso, StateStore};
use super::webdav::{WebDavBackend, WebDavConfig};
use super::{SyncEngine, SyncSummary};
use crate::settings::{Settings, SyncSettings};
use crate::AppState;

/// 保存后等这么久再同步，连续记录只跑一轮。
const DEBOUNCE: Duration = Duration::from_secs(5);
/// 启动后等这么久做首轮，别和启动动画抢磁盘和网络。
const STARTUP_DELAY: Duration = Duration::from_secs(60);
/// 单轮请求预算；用尽后剩余文件留到下一轮，避免触发坚果云限流。
const REQUEST_BUDGET: usize = 200;

/// 前端展示用的同步状态。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SyncStatus {
    /// 地址与用户名都填了。
    pub configured: bool,
    pub enabled: bool,
    pub running: bool,
    pub last_sync_at: Option<String>,
    pub last_error: Option<String>,
    /// 降级说明；正常时为 None。
    pub degraded: Option<String>,
    pub summary: Option<SyncSummary>,
    /// 笔记目录里的冲突副本数量。
    pub conflicts: usize,
}

#[derive(Default)]
struct LastRound {
    last_sync_at: Option<String>,
    last_error: Option<String>,
    degraded: Option<String>,
    summary: Option<SyncSummary>,
}

pub(crate) struct SyncRuntime {
    /// 调度线程在这里等触发；保存、改配置、托盘点击都会 `notify_one`。
    wake: Notify,
    running: AtomicBool,
    pending: AtomicBool,
    stopping: AtomicBool,
    /// 凭据管理器不可用时，密码只放在这里，退出即丢。
    session_password: Mutex<Option<String>>,
    last: Mutex<LastRound>,
}

impl SyncRuntime {
    pub(crate) fn new() -> Self {
        Self {
            wake: Notify::new(),
            running: AtomicBool::new(false),
            pending: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            session_password: Mutex::new(None),
            last: Mutex::new(LastRound::default()),
        }
    }

    /// 请求在防抖窗口后跑一轮。
    pub(crate) fn schedule(&self) {
        self.wake.notify_one();
    }

    pub(crate) fn stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        self.wake.notify_one();
    }

    pub(crate) fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub(crate) fn remember_password(&self, password: &str) {
        *lock(&self.session_password) = Some(password.to_string());
    }

    pub(crate) fn forget_password(&self) {
        *lock(&self.session_password) = None;
    }

    pub(crate) fn cached_password(&self) -> Option<String> {
        lock(&self.session_password).clone()
    }

    fn apply(&self, outcome: RoundOutcome) {
        let mut last = lock(&self.last);
        match outcome {
            RoundOutcome::Skipped => {}
            RoundOutcome::Failed(error) => {
                last.last_error = Some(error);
            }
            RoundOutcome::Done(summary, degraded) => {
                last.last_sync_at = summary.finished_at.clone().or_else(|| Some(now_iso()));
                last.last_error = summary.errors.first().cloned();
                last.degraded = degraded;
                last.summary = Some(summary);
            }
        }
    }
}

impl Default for SyncRuntime {
    fn default() -> Self {
        Self::new()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

enum RoundOutcome {
    /// 未启用或未配置：什么都不做，也不覆盖上一次的错误。
    Skipped,
    Failed(String),
    Done(SyncSummary, Option<String>),
}

/// 启动调度线程。只在 `main` 的 setup 里调用一次。
pub(crate) fn start(app: &AppHandle) {
    let handle = app.clone();
    let runtime = app.state::<AppState>().sync.clone();

    tauri::async_runtime::spawn(async move {
        // 启动延迟期间也接受触发：用户一启用同步就不用再等满 60 秒。
        tokio::select! {
            _ = tokio::time::sleep(STARTUP_DELAY) => {}
            _ = runtime.wake.notified() => {
                tokio::time::sleep(DEBOUNCE).await;
            }
        }

        loop {
            if runtime.stopping.load(Ordering::SeqCst) {
                break;
            }

            let sync = settings_snapshot(&handle).sync;
            if sync.enabled && sync.is_configured() {
                run_round(handle.clone()).await;
            }

            let wait = Duration::from_secs(u64::from(sync.interval_minutes.clamp(1, 60)) * 60);
            let trigger = runtime.wake.notified();
            tokio::pin!(trigger);
            // 先登记再等待：等待期间到达的触发不会丢。
            trigger.as_mut().enable();

            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = trigger.as_mut() => {
                    tokio::time::sleep(DEBOUNCE).await;
                }
            }
        }
    });
}

/// 立即触发一轮；已在跑时并入当前轮，等它结束后补一轮。
pub(crate) fn trigger(app: &AppHandle) {
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        run_round(handle).await;
    });
}

/// 跑一轮（单飞）。重复调用安全。
pub(crate) async fn run_round(app: AppHandle) {
    let runtime = match app.try_state::<AppState>() {
        Some(state) => state.sync.clone(),
        None => return,
    };

    if runtime.running.swap(true, Ordering::SeqCst) {
        runtime.pending.store(true, Ordering::SeqCst);
        return;
    }

    emit_status(&app).await;
    loop {
        let outcome = round(&app, &runtime).await;
        runtime.apply(outcome);
        if !runtime.pending.swap(false, Ordering::SeqCst) {
            break;
        }
    }
    runtime.running.store(false, Ordering::SeqCst);
    // 收尾时又来的触发不能丢：补跑一次。
    if runtime.pending.swap(false, Ordering::SeqCst) {
        trigger(&app);
    }
    emit_status(&app).await;
}

async fn round(app: &AppHandle, runtime: &SyncRuntime) -> RoundOutcome {
    let (settings, io) = match app.try_state::<AppState>() {
        Some(state) => (lock(&state.settings).clone(), state.io.clone()),
        None => return RoundOutcome::Skipped,
    };

    let sync = settings.sync.clone();
    if !sync.enabled || !sync.is_configured() {
        return RoundOutcome::Skipped;
    }

    let password = match resolve_password(runtime, &sync) {
        Ok(password) => password,
        Err(error) => return RoundOutcome::Failed(error),
    };

    let config = WebDavConfig {
        url: sync.url.clone(),
        username: sync.username.clone(),
        password,
        allow_insecure_http: sync.allow_insecure_http,
    };
    let backend = match WebDavBackend::connect(config).await {
        Ok(backend) => backend,
        Err(error) => return RoundOutcome::Failed(error.to_string()),
    };

    let degraded = capability_note(&backend.capabilities());
    backend.reset_budget(REQUEST_BUDGET);

    let config_dir = crate::settings::settings_path(app)
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let notes_dir = PathBuf::from(&settings.notes_dir);
    // 只差末尾斜杠的地址是同一个远端，状态目录不能因此换一份。
    let remote_key = format!(
        "{}\n{}",
        sync.url.trim_end_matches('/'),
        sync.username.trim()
    );
    let store = StateStore::with_remote(&config_dir, &notes_dir, &remote_key);
    let mut engine = SyncEngine::new(backend, store, notes_dir, &sync.device_label(), io);

    RoundOutcome::Done(engine.run().await, degraded)
}

fn resolve_password(runtime: &SyncRuntime, sync: &SyncSettings) -> Result<String, String> {
    if let Some(password) = runtime.cached_password() {
        return Ok(password);
    }
    match secrets::load(&sync.url, &sync.username) {
        Ok(Some(password)) => {
            runtime.remember_password(&password);
            Ok(password)
        }
        Ok(None) => Err("还没有保存同步密码，请在设置里重新填写".to_string()),
        Err(error) => Err(error),
    }
}

/// 能力不足时给用户一句人话；`None` 表示可以安全传播删除。
pub(crate) fn capability_note(caps: &Capabilities) -> Option<String> {
    if caps.supports_deletion() {
        return None;
    }
    let mut reasons = Vec::new();
    if !caps.strong_etag {
        reasons.push("没有强 ETag");
    }
    if !caps.honors_if_match {
        reasons.push("忽略条件写入");
    }
    if !caps.reliable_listing {
        reasons.push("目录列表不可靠");
    }
    Some(format!(
        "已连接（降级：{}，不会传播删除）",
        reasons.join("、")
    ))
}

/// 当前状态；笔记目录里的冲突副本数量会现扫一遍。
pub(crate) fn status(app: &AppHandle) -> SyncStatus {
    let settings = settings_snapshot(app);
    let runtime = app.state::<AppState>().sync.clone();
    let last = lock(&runtime.last);
    SyncStatus {
        configured: settings.sync.is_configured(),
        enabled: settings.sync.enabled,
        running: runtime.is_running(),
        last_sync_at: last.last_sync_at.clone(),
        last_error: last.last_error.clone(),
        degraded: last.degraded.clone(),
        summary: last.summary.clone(),
        conflicts: count_conflicts(&PathBuf::from(&settings.notes_dir)),
    }
}

/// 状态计算会扫描笔记目录；必须放进阻塞线程池，不能卡住调用方。
pub(crate) async fn emit_status(app: &AppHandle) {
    let handle = app.clone();
    let snapshot = match tauri::async_runtime::spawn_blocking(move || status(&handle)).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            log::warn!("计算同步状态失败：{error}");
            return;
        }
    };
    if let Err(error) = app.emit("sync-status", snapshot) {
        log::warn!("广播同步状态失败：{error}");
    }
}

/// 冲突副本：`YYYY-MM-DD.conflict-<设备>-<时间>.md`。
fn count_conflicts(dir: &std::path::Path) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().contains(".conflict-"))
        .count()
}

fn settings_snapshot(app: &AppHandle) -> Settings {
    let state = app.state::<AppState>();
    let settings = lock(&state.settings).clone();
    settings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strong() -> Capabilities {
        Capabilities {
            strong_etag: true,
            honors_if_match: true,
            reliable_listing: true,
        }
    }

    #[test]
    fn strong_server_has_no_degradation_note() {
        assert!(capability_note(&strong()).is_none());
    }

    #[test]
    fn weak_server_explains_that_deletion_is_off() {
        let note = capability_note(&Capabilities::default()).expect("弱服务器应给出降级说明");
        assert!(note.contains("降级"), "{note}");
        assert!(note.contains("不会传播删除"), "{note}");
        assert!(note.contains("ETag"), "{note}");
    }

    #[test]
    fn session_password_is_memory_only() {
        let runtime = SyncRuntime::new();
        assert!(runtime.cached_password().is_none());
        runtime.remember_password("s3cret");
        assert_eq!(runtime.cached_password().as_deref(), Some("s3cret"));
        runtime.forget_password();
        assert!(runtime.cached_password().is_none());
    }

    #[test]
    fn conflicts_are_counted_from_note_names() {
        let dir =
            std::env::temp_dir().join(format!("flashnote-sync-conflicts-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("2026-10-23.md"), "").unwrap();
        fs::write(dir.join("2026-10-23.conflict-pc-20261023101010.md"), "").unwrap();
        fs::write(dir.join("2026-10-22.conflict-pc-20261022090000.md"), "").unwrap();
        assert_eq!(count_conflicts(&dir), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_serializes_as_camel_case() {
        let status = SyncStatus {
            configured: true,
            enabled: true,
            running: false,
            last_sync_at: Some("2026-10-23T09:00:00Z".into()),
            last_error: None,
            degraded: None,
            summary: None,
            conflicts: 1,
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"lastSyncAt\""), "{json}");
        assert!(json.contains("\"conflicts\":1"), "{json}");
    }
}
