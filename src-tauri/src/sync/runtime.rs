//! 同步运行时：配置快照、单飞、调度与状态广播。
//!
//! `SyncEngine` 只负责一轮的具体动作；这里决定「什么时候跑」，并把结果变成
//! 前端可见的 `SyncStatus`。调度规则见计划：保存后防抖 5 秒、启动 60 秒后首轮、
//! 之后按 5/15/60 分钟轮询，手动触发立即执行。
//!
//! 并发约定：同一时刻只有一轮在跑；跑的过程中来的触发只记一个「待办」，
//! 当前轮结束后再补一轮，绝不叠加。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Notify;

use super::backend::{Capabilities, SyncBackend, SyncError};
use super::github::{self, GitHubBackend, GitHubConfig};
use super::secrets;
use super::state::{now_iso, sha256_base64, StateStore, SyncState};
use super::webdav::{WebDavBackend, WebDavConfig};
use super::{SyncEngine, SyncSummary};
use crate::settings::{Settings, SyncKind, SyncSettings};
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

    let notes_dir = PathBuf::from(&settings.notes_dir);

    match sync.kind {
        SyncKind::Github => {
            let Some((owner, repo, url_branch)) = github::parse_repo(&sync.url) else {
                return RoundOutcome::Failed(
                    "GitHub 仓库地址格式不正确，应形如 https://github.com/owner/repo".to_string(),
                );
            };
            let config = GitHubConfig {
                owner,
                repo,
                // 地址里写了 /tree/<分支> 就以它为准，其次才是表单里的分支
                branch: url_branch.or_else(|| sync.github_branch()),
                token: password,
            };
            let backend = GitHubBackend::connect(config).await;
            drive(app, &sync, notes_dir, io, backend).await
        }
        SyncKind::Webdav => {
            let config = WebDavConfig {
                url: sync.url.clone(),
                username: sync.username.clone(),
                password,
                allow_insecure_http: sync.allow_insecure_http,
            };
            let backend = WebDavBackend::connect(config).await;
            drive(app, &sync, notes_dir, io, backend).await
        }
    }
}

/// 两种后端共用的一轮：探测能力、建状态目录、跑引擎、补一次 Git 提交。
async fn drive<B: SyncBackend + 'static>(
    app: &AppHandle,
    sync: &SyncSettings,
    notes_dir: PathBuf,
    io: Arc<Mutex<()>>,
    backend: Result<B, SyncError>,
) -> RoundOutcome {
    let backend = match backend {
        Ok(backend) => backend,
        Err(error) => return RoundOutcome::Failed(error.to_string()),
    };

    let degraded = capability_note(&backend.capabilities());
    backend.reset_budget(REQUEST_BUDGET);

    let config_dir = crate::settings::settings_path(app)
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    // 只差末尾斜杠、大小写或 query 的地址是同一个远端，状态目录不能因此换一份。
    let remote_key = super::remote_identity(sync.kind, &sync.url, &sync.username);
    // 0.5.0 用原始 URL 做身份；旧目录存在时直接复用，避免升级后全量重合并。
    let legacy_key = format!(
        "{}\n{}",
        sync.url.trim().trim_end_matches('/'),
        sync.username.trim()
    );
    let store =
        StateStore::with_remote_migrating(&config_dir, &notes_dir, &remote_key, &legacy_key);
    let device = sync.device_label();
    // StateStore::load 会读 state.json；连同引擎构造一起放进阻塞线程池。
    let mut engine = match tauri::async_runtime::spawn_blocking(move || {
        SyncEngine::new(backend, store, notes_dir, &device, io)
    })
    .await
    {
        Ok(engine) => engine,
        Err(error) => return RoundOutcome::Failed(format!("同步引擎初始化失败：{error}")),
    };

    let summary = engine.run().await;
    // 下载、合并和冲突副本都会在本地产生新内容；补一次提交，Git 历史才不会断档。
    // 本地保存/删除已经由各自的命令提交过，这里重复调用只会命中「没有变化」。
    if summary.downloaded + summary.merged + summary.conflicts > 0 {
        crate::git_backup::schedule(app);
    }
    RoundOutcome::Done(summary, degraded)
}

fn resolve_password(runtime: &SyncRuntime, sync: &SyncSettings) -> Result<String, String> {
    if let Some(password) = runtime.cached_password() {
        return Ok(password);
    }
    match secrets::load(sync.kind, &sync.url, &sync.username) {
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

/// 历史面板里某一天文件的同步状态。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FileSyncState {
    pub date: String,
    /// `synced` 已同步 / `pending` 待同步 / `conflict` 有冲突副本。
    pub state: &'static str,
}

/// 日期会被拼进文件名，只接受 `YYYY-MM-DD`，其余一律丢弃。
fn is_date_name(date: &str) -> bool {
    let bytes = date.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
}

/// 计算指定日期文件的同步状态。
///
/// 状态取自 `state.json` 的 `local_sha256` 与磁盘内容比对：一致才算已同步。
/// 没同步过、同步后又被改过都算待同步——条目级不单独同步，不给出更细的结论。
pub(crate) fn file_states(app: &AppHandle, dates: &[String]) -> Vec<FileSyncState> {
    let settings = settings_snapshot(app);
    if !settings.sync.enabled || !settings.sync.is_configured() {
        return Vec::new();
    }
    let Some(config_dir) = crate::settings::settings_path(app)
        .parent()
        .map(PathBuf::from)
    else {
        return Vec::new();
    };
    let notes_dir = PathBuf::from(&settings.notes_dir);
    let remote_key = super::remote_identity(
        settings.sync.kind,
        &settings.sync.url,
        &settings.sync.username,
    );
    let legacy_key = format!(
        "{}\n{}",
        settings.sync.url.trim().trim_end_matches('/'),
        settings.sync.username.trim()
    );
    let store =
        StateStore::with_remote_migrating(&config_dir, &notes_dir, &remote_key, &legacy_key);
    // 状态读不出来时宁可不显示，也不要把「读不到」画成「未同步」。
    let Ok((state, _)) = store.load() else {
        return Vec::new();
    };
    file_states_for(&notes_dir, &state, dates)
}

/// 纯函数版本，便于测试：按状态记录与磁盘内容比对，不访问网络。
pub(crate) fn file_states_for(
    notes_dir: &Path,
    state: &SyncState,
    dates: &[String],
) -> Vec<FileSyncState> {
    let conflicts = conflict_dates(notes_dir);
    let mut out = Vec::with_capacity(dates.len());
    for date in dates {
        if !is_date_name(date) {
            continue;
        }
        if conflicts.iter().any(|value| value == date) {
            out.push(FileSyncState {
                date: date.clone(),
                state: "conflict",
            });
            continue;
        }
        let name = format!("{date}.md");
        let Ok(bytes) = fs::read(notes_dir.join(&name)) else {
            continue;
        };
        let sha = sha256_base64(&bytes);
        let value = match state.files.get(&name) {
            Some(record) if record.local_sha256 == sha => "synced",
            _ => "pending",
        };
        out.push(FileSyncState {
            date: date.clone(),
            state: value,
        });
    }
    out
}

/// 有冲突副本的日期（`YYYY-MM-DD.conflict-*.md` 的文件名前缀）。
fn conflict_dates(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let (date, rest) = name.split_once(".conflict-")?;
            (rest.ends_with(".md") && is_date_name(date)).then(|| date.to_string())
        })
        .collect()
}

fn settings_snapshot(app: &AppHandle) -> Settings {
    let state = app.state::<AppState>();
    let settings = lock(&state.settings).clone();
    settings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::backend::RemoteVersion;
    use crate::sync::state::FileRecord;

    fn strong() -> Capabilities {
        Capabilities {
            strong_etag: true,
            honors_if_match: true,
            honors_if_none_match: true,
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
    fn file_states_compare_content_with_the_last_synced_hash() {
        let dir = std::env::temp_dir().join(format!("flashnote-sync-files-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("2026-10-23.md"), "hello").unwrap();
        fs::write(dir.join("2026-10-22.md"), "old").unwrap();
        fs::write(dir.join("2026-10-22.conflict-pc-20261022100000.md"), "x").unwrap();

        let mut state = SyncState::default();
        state.files.insert(
            "2026-10-23.md".into(),
            FileRecord {
                local_mtime_ns: 0,
                local_size: 5,
                local_sha256: sha256_base64(b"hello"),
                remote_version: RemoteVersion::Etag("\"v1\"".into()),
                remote_sha256: sha256_base64(b"hello"),
                synced_at: now_iso(),
            },
        );

        let dates = vec![
            "2026-10-23".to_string(),
            "2026-10-22".to_string(),
            // 真实日期格式之外的值一律丢弃，不能被拼进文件名
            "../../etc".to_string(),
        ];
        assert_eq!(
            file_states_for(&dir, &state, &dates),
            vec![
                FileSyncState {
                    date: "2026-10-23".into(),
                    state: "synced",
                },
                FileSyncState {
                    date: "2026-10-22".into(),
                    state: "conflict",
                },
            ]
        );

        // 同步之后又改过内容：回到「待同步」
        fs::write(dir.join("2026-10-23.md"), "changed").unwrap();
        let states = file_states_for(&dir, &state, &["2026-10-23".to_string()]);
        assert_eq!(states[0].state, "pending");

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
