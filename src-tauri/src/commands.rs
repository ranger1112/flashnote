//! 前后端命令：设置、窗口显隐、笔记读写与草稿。
//!
//! 文件 I/O 一律通过 `run_blocking` 放进阻塞线程池，设置持久化则交给
//! 非阻塞的 `persist`：UI 线程与 tokio worker 都不该等磁盘。

use crate::{
    files::{self, io_guard},
    notes,
    settings::{self, Settings, SyncSettings},
    sync::{self, backend::SyncBackend, runtime::SyncStatus, webdav},
    AppState,
};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_global_shortcut::GlobalShortcutExt;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShowPayload {
    pub source: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedNote {
    pub path: String,
    pub time: String,
}

/// 把阻塞文件 I/O 放进专用阻塞线程池，避免占用 UI 线程或 tokio worker。
async fn run_blocking<T, F>(task: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(task)
        .await
        .map_err(|err| format!("后台任务失败：{err}"))?
}

/// 非阻塞地持久化当前设置：UI 线程只复制快照，真正写盘在阻塞线程池里完成。
/// 快照带递增序号，并发写入时最终落盘的总是最新一份。
pub fn persist(app: &AppHandle) {
    let state = app.state::<AppState>();
    let (snapshot, seq) = {
        let settings = state
            .settings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let seq = state.settings_write_seq.fetch_add(1, Ordering::SeqCst) + 1;
        (settings.clone(), seq)
    };
    let path = state.settings_path.clone();
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = handle.state::<AppState>();
        write_snapshot(&state, &path, seq, &snapshot);
    });
}

/// 退出路径同步落盘，确保进程结束前最后一次修改不会丢。
pub fn persist_sync(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let (snapshot, seq) = {
        let settings = state
            .settings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let seq = state.settings_write_seq.fetch_add(1, Ordering::SeqCst) + 1;
        (settings.clone(), seq)
    };
    if write_snapshot(&state, &state.settings_path, seq, &snapshot) {
        Ok(())
    } else {
        Err("设置（含同步状态）写入失败".to_string())
    }
}

fn write_snapshot(state: &AppState, path: &Path, seq: u64, snapshot: &Settings) -> bool {
    write_snapshot_locked(
        path,
        seq,
        snapshot,
        &state.settings_last_saved_seq,
        &state.settings_write_lock,
    )
}

/// 写盘成功后才推进 `last_saved`：这样只有「真正落盘的更新快照」会让旧快照跳过。
/// 若最新一次写入失败，排在后面的旧快照仍会兜底，不会把两次修改一起丢掉。
fn write_snapshot_locked(
    path: &Path,
    seq: u64,
    snapshot: &Settings,
    last_saved: &AtomicU64,
    write_lock: &Mutex<()>,
) -> bool {
    let _guard = write_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if seq <= last_saved.load(Ordering::SeqCst) {
        return false;
    }
    match settings::save(path, snapshot) {
        Ok(()) => {
            last_saved.store(seq, Ordering::SeqCst);
            true
        }
        Err(err) => {
            log::error!("保存设置失败：{err}");
            false
        }
    }
}

/// 退出时补存窗口几何与设置。
pub fn persist_on_exit(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        crate::geometry::persist_geometry(app, &window);
    }
    // 先让调度线程停下，避免退出过程中再起一轮同步。
    app.state::<AppState>().sync.stop();
    let _ = persist_sync(app);
}

pub fn show_window_impl(app: &AppHandle) {
    let state = app.state::<AppState>();
    let capture = state
        .settings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .capture_source;

    let source = if capture {
        match crate::source::foreground_window_title() {
            Some(title) => {
                *state
                    .last_source
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(title.clone());
                Some(title)
            }
            // 托盘菜单会短暂抢走前台窗口，抓不到时沿用上一次的来源
            None => state
                .last_source
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone(),
        }
    } else {
        None
    };

    if let Some(window) = app.get_webview_window("main") {
        if let Err(err) = window.show() {
            log::error!("显示窗口失败：{err}");
            return;
        }
        if let Err(err) = window.unminimize() {
            log::warn!("取消最小化失败：{err}");
        }
        if let Err(err) = window.set_focus() {
            log::warn!("聚焦窗口失败：{err}");
        }
        if let Err(err) = app.emit("window-shown", ShowPayload { source }) {
            log::warn!("发送 window-shown 事件失败：{err}");
        }
    }
}

pub fn hide_window_impl(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        crate::geometry::persist_geometry(app, &window);
        persist(app);
        if let Err(err) = window.hide() {
            log::error!("隐藏窗口失败：{err}");
        }
    }
}

pub fn toggle_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        if window.is_visible().unwrap_or(false) {
            hide_window_impl(app);
        } else {
            show_window_impl(app);
        }
    }
}

/// 用系统默认浏览器打开预览里的链接。
#[tauri::command]
pub fn open_external_url(url: String) -> Result<(), String> {
    let target = url.trim();
    if !files::external_url_allowed(target) {
        return Err("只允许打开 http(s) 或 mailto 链接".to_string());
    }
    files::open_with_shell(target)
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Settings {
    state
        .settings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

#[tauri::command]
pub async fn save_note(app: AppHandle, content: String) -> Result<SavedNote, String> {
    let content = content.trim().to_string();
    if content.is_empty() {
        return Err("内容为空".to_string());
    }

    run_blocking(move || {
        let state = app.state::<AppState>();
        let (settings, source_title) = {
            let settings = state
                .settings
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            let source_title = if settings.capture_source {
                state
                    .last_source
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone()
            } else {
                None
            };
            (settings, source_title)
        };

        let (date, time) = notes::now_parts();
        let entry = if settings.markdown_mode {
            notes::format_entry_markdown(&time, &content, source_title.as_deref())
        } else {
            notes::format_entry(&time, &content, source_title.as_deref())
        };
        let dir = PathBuf::from(&settings.notes_dir);
        let path = {
            let _guard = io_guard(&state);
            notes::append_note(&dir, &date, &entry).map_err(|err| format!("写入失败：{err}"))?
        };
        // 落盘成功后再排一轮同步：防抖 5 秒，连续记录只跑一次。
        state.sync.schedule();

        Ok(SavedNote {
            path: path.to_string_lossy().to_string(),
            time,
        })
    })
    .await
}

#[tauri::command]
pub fn set_pinned(app: AppHandle, state: State<'_, AppState>, pinned: bool) -> Result<(), String> {
    state
        .settings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .pinned = pinned;
    persist(&app);
    Ok(())
}

#[tauri::command]
pub async fn update_notes_dir(app: AppHandle, path: String) -> Result<(), String> {
    // 网络目录掉线时 create_dir_all 会阻塞很久：探测不能持 io_guard，
    // 否则所有笔记读写都会排在后面等这次超时。
    let dir = run_blocking(move || files::ensure_writable_dir(&path)).await?;
    {
        let state = app.state::<AppState>();
        state
            .settings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .notes_dir = dir.to_string_lossy().to_string();
        // 目录变了等于换了一份同步状态，让调度下一轮按新目录重新开始。
        state.sync.schedule();
    }
    persist(&app);
    log::info!("笔记目录已更新为 {}", dir.display());
    Ok(())
}

#[tauri::command]
pub fn update_hotkey(
    app: AppHandle,
    state: State<'_, AppState>,
    accel: String,
) -> Result<(), String> {
    let shortcut = crate::parse_hotkey(&accel)?;

    let old = state
        .settings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .hotkey
        .clone();
    if let Err(err) = app.global_shortcut().unregister_all() {
        log::warn!("取消旧快捷键失败：{err}");
    }

    match app.global_shortcut().register(shortcut) {
        Ok(()) => {
            state
                .settings
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .hotkey = accel.clone();
            persist(&app);
            log::info!("全局热键已更新为 {accel}");
            Ok(())
        }
        Err(err) => {
            log::warn!("注册新快捷键 {accel} 失败：{err}");
            if let Ok(previous) = crate::parse_hotkey(&old) {
                if let Err(rollback_err) = app.global_shortcut().register(previous) {
                    log::error!("恢复旧快捷键 {old} 失败：{rollback_err}");
                }
            }
            Err(format!("注册失败（可能被其他程序占用）：{err}"))
        }
    }
}

#[tauri::command]
pub fn update_capture_source(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<(), String> {
    state
        .settings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .capture_source = enabled;
    if !enabled {
        *state
            .last_source
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }
    persist(&app);
    Ok(())
}

#[tauri::command]
pub fn update_markdown_mode(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<(), String> {
    state
        .settings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .markdown_mode = enabled;
    persist(&app);
    Ok(())
}

#[tauri::command]
pub async fn list_notes(
    app: AppHandle,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Vec<notes::NoteEntry>, String> {
    let dir = {
        let state = app.state::<AppState>();
        let notes_dir = state
            .settings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .notes_dir
            .clone();
        PathBuf::from(notes_dir)
    };
    let limit = clamp_limit(limit);
    let offset = offset.unwrap_or(0) as usize;

    run_blocking(move || {
        notes::load_recent(&dir, limit, offset).map_err(|err| format!("读取笔记失败：{err}"))
    })
    .await
}

#[tauri::command]
pub async fn search_notes(
    app: AppHandle,
    query: String,
    limit: Option<u32>,
) -> Result<Vec<notes::NoteEntry>, String> {
    let dir = {
        let state = app.state::<AppState>();
        let notes_dir = state
            .settings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .notes_dir
            .clone();
        PathBuf::from(notes_dir)
    };
    let limit = clamp_limit(limit);

    run_blocking(move || {
        notes::search_entries(&dir, &query, limit).map_err(|err| format!("搜索笔记失败：{err}"))
    })
    .await
}

/// 列表条数限制在 1..=1000，缺省 200。
fn clamp_limit(limit: Option<u32>) -> usize {
    limit.unwrap_or(200).clamp(1, 1000) as usize
}

#[tauri::command]
pub async fn delete_note_entry(
    app: AppHandle,
    path: String,
    date: String,
    time: String,
    content: String,
) -> Result<(), String> {
    run_blocking(move || {
        let state = app.state::<AppState>();
        let result = files::rewrite_note(&state, &path, |text| {
            notes::remove_entry(text, &date, &time, &content)
        });
        if result.is_ok() {
            state.sync.schedule();
        }
        result
    })
    .await
}

#[tauri::command]
pub async fn update_note_entry(
    app: AppHandle,
    path: String,
    date: String,
    time: String,
    content: String,
    new_content: String,
) -> Result<(), String> {
    if new_content.trim().is_empty() {
        return Err("内容不能为空".to_string());
    }
    run_blocking(move || {
        let state = app.state::<AppState>();
        let result = files::rewrite_note(&state, &path, |text| {
            notes::replace_entry(text, &date, &time, &content, &new_content)
        });
        if result.is_ok() {
            state.sync.schedule();
        }
        result
    })
    .await
}

#[tauri::command]
pub async fn open_note_file(app: AppHandle, path: String) -> Result<(), String> {
    run_blocking(move || {
        let state = app.state::<AppState>();
        files::open_note_file_impl(&state, &path)
    })
    .await
}

#[tauri::command]
pub async fn open_today_note(app: AppHandle) -> Result<(), String> {
    run_blocking(move || files::open_today_note_impl(&app)).await
}

#[tauri::command]
pub async fn open_notes_dir(app: AppHandle) -> Result<(), String> {
    run_blocking(move || files::open_notes_dir_impl(&app)).await
}

#[tauri::command]
pub fn hide_window(app: AppHandle) {
    hide_window_impl(&app);
}

#[tauri::command]
pub fn take_startup_notice(state: State<'_, AppState>) -> Option<crate::StartupNotice> {
    state
        .startup_notice
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
}

fn draft_path(app: &AppHandle) -> PathBuf {
    settings::settings_path(app).with_file_name("draft.txt")
}

#[tauri::command]
pub async fn load_draft(app: AppHandle) -> Result<String, String> {
    run_blocking(move || match std::fs::read_to_string(draft_path(&app)) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(format!("读取草稿失败：{err}")),
    })
    .await
}

#[tauri::command]
pub async fn save_draft(app: AppHandle, content: String) -> Result<(), String> {
    run_blocking(move || {
        let state = app.state::<AppState>();
        let _guard = io_guard(&state);
        let path = draft_path(&app);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| format!("保存草稿失败：{err}"))?;
        }
        let tmp = path.with_extension("txt.tmp");
        std::fs::write(&tmp, content.as_bytes()).map_err(|err| format!("保存草稿失败：{err}"))?;
        std::fs::rename(&tmp, &path).map_err(|err| format!("保存草稿失败：{err}"))
    })
    .await
}

#[tauri::command]
pub async fn clear_draft(app: AppHandle) -> Result<(), String> {
    run_blocking(move || {
        let state = app.state::<AppState>();
        let _guard = io_guard(&state);
        match std::fs::remove_file(draft_path(&app)) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(format!("清除草稿失败：{err}")),
        }
    })
    .await
}

#[tauri::command]
pub fn complete_onboarding(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    state
        .settings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .onboarded = true;
    persist(&app);
    Ok(())
}

#[tauri::command]
pub fn update_history_preview(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<(), String> {
    state
        .settings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .history_preview = enabled;
    persist(&app);
    Ok(())
}

#[tauri::command]
pub fn update_auto_update(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<(), String> {
    state
        .settings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .auto_update = enabled;
    persist(&app);
    Ok(())
}

#[tauri::command]
pub fn update_theme(app: AppHandle, state: State<'_, AppState>, theme: String) -> String {
    let applied = settings::normalize_theme(&theme);
    state
        .settings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .theme = applied.clone();
    persist(&app);
    applied
}

#[tauri::command]
pub fn update_auto_hide(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<(), String> {
    state
        .settings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .auto_hide_on_blur = enabled;
    persist(&app);
    Ok(())
}

#[tauri::command]
pub fn update_background_opacity(app: AppHandle, state: State<'_, AppState>, opacity: f64) -> f64 {
    let applied = settings::clamp_opacity(opacity);
    state
        .settings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .background_opacity = applied;
    persist(&app);
    applied
}

/// 前端提交的同步配置。密码不在这里，走 `save_sync_password`。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncConfigInput {
    pub enabled: bool,
    pub url: String,
    pub username: String,
    pub interval_minutes: u32,
    pub allow_insecure_http: bool,
    pub device_name: String,
}

impl SyncConfigInput {
    /// 校验并规范化；启用时必须填全地址与用户名。
    fn normalize(self) -> Result<SyncSettings, String> {
        let url = self.url.trim().to_string();
        let username = self.username.trim().to_string();
        if !url.is_empty() {
            webdav::validate_url(&url, self.allow_insecure_http)
                .map_err(|error| error.to_string())?;
        }
        if self.enabled && (url.is_empty() || username.is_empty()) {
            return Err("启用同步前请填写 WebDAV 地址和用户名".to_string());
        }
        Ok(SyncSettings {
            enabled: self.enabled,
            url,
            username,
            interval_minutes: SyncSettings::normalize_interval(self.interval_minutes),
            allow_insecure_http: self.allow_insecure_http,
            device_name: self.device_name.trim().to_string(),
        })
    }
}

/// 保存密码的结果：凭据管理器不可用时只在本次会话内有效。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordSaveResult {
    pub persistent: bool,
    pub warning: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncTestResult {
    pub files: usize,
    pub degraded: Option<String>,
    pub message: String,
}

#[tauri::command]
pub async fn get_sync_status(app: AppHandle) -> Result<SyncStatus, String> {
    run_blocking(move || Ok(sync::runtime::status(&app))).await
}

/// 两个同步配置是否指向同一远端账号；只差 URL 末尾斜杠视为相同。
fn same_sync_identity(left: &settings::SyncSettings, right: &settings::SyncSettings) -> bool {
    // 与凭据、状态目录共用同一份身份：大小写/尾斜杠/query 的差异不算换远端。
    sync::remote_identity(&left.url, &left.username)
        == sync::remote_identity(&right.url, &right.username)
}

#[tauri::command]
pub async fn update_sync_config(
    app: AppHandle,
    state: State<'_, AppState>,
    config: SyncConfigInput,
) -> Result<SyncStatus, String> {
    let next = config.normalize()?;
    let previous = {
        let mut settings = state
            .settings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = settings.sync.clone();
        settings.sync = next.clone();
        previous
    };

    // 换服务器或换账号后旧密码不再适用，顺手清掉，免得留下孤儿凭据。
    if !same_sync_identity(&previous, &next) {
        state.sync.forget_password();
        if previous.is_configured() {
            if let Err(error) = sync::secrets::clear(&previous.url, &previous.username) {
                log::warn!("清理旧同步密码失败：{error}");
            }
        }
    }

    persist(&app);
    state.sync.schedule();
    sync::runtime::emit_status(&app).await;
    // 状态计算会扫描笔记目录，放回阻塞线程池再返回。
    let handle = app.clone();
    run_blocking(move || Ok(sync::runtime::status(&handle))).await
}

#[tauri::command]
pub fn save_sync_password(
    state: State<'_, AppState>,
    password: String,
) -> Result<PasswordSaveResult, String> {
    if password.is_empty() {
        return Err("密码不能为空".to_string());
    }
    let sync = state
        .settings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .sync
        .clone();
    if !sync.is_configured() {
        return Err("请先填写 WebDAV 地址和用户名".to_string());
    }

    // 先记在内存里：凭据管理器被禁用时，本次会话仍然能同步。
    state.sync.remember_password(&password);
    // 密码补上后立刻可以同步，不用等下一个定时点。
    state.sync.schedule();
    match sync::secrets::save(&sync.url, &sync.username, &password) {
        Ok(()) => Ok(PasswordSaveResult {
            persistent: true,
            warning: None,
        }),
        Err(error) => {
            log::warn!("保存同步密码到凭据管理器失败：{error}");
            Ok(PasswordSaveResult {
                persistent: false,
                warning: Some(format!("{error}；本次会话仍可同步，退出后需要重新输入")),
            })
        }
    }
}

#[tauri::command]
pub async fn clear_sync_password(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let sync = state
        .settings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .sync
        .clone();
    state.sync.forget_password();
    if sync.is_configured() {
        sync::secrets::clear(&sync.url, &sync.username)?;
    }
    sync::runtime::emit_status(&app).await;
    Ok(())
}

/// 测试连接：只探测与列目录，不改动本地状态。
#[tauri::command]
pub async fn test_sync_connection(
    app: AppHandle,
    config: SyncConfigInput,
    password: Option<String>,
) -> Result<SyncTestResult, String> {
    let sync = config.normalize()?;
    if !sync.is_configured() {
        return Err("请先填写 WebDAV 地址和用户名".to_string());
    }

    let saved = app
        .state::<AppState>()
        .settings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .sync
        .clone();
    let same_identity = same_sync_identity(&saved, &sync);
    let runtime = app.state::<AppState>().sync.clone();
    let password = match password.filter(|value| !value.is_empty()) {
        Some(password) => password,
        // 表单地址与已保存配置不一致时，绝不能把缓存里的旧密码发给新地址；
        // 只允许使用新地址自己在凭据管理器里保存过的密码。
        None if same_identity => match runtime.cached_password() {
            Some(password) => password,
            None => sync::secrets::load(&sync.url, &sync.username)?
                .ok_or_else(|| "还没有保存密码".to_string())?,
        },
        None => sync::secrets::load(&sync.url, &sync.username)?
            .ok_or_else(|| "地址或用户名已修改，请重新输入密码".to_string())?,
    };

    let backend = webdav::WebDavBackend::connect(webdav::WebDavConfig {
        url: sync.url.clone(),
        username: sync.username.clone(),
        password,
        allow_insecure_http: sync.allow_insecure_http,
    })
    .await
    .map_err(|error| error.to_string())?;

    let degraded = sync::runtime::capability_note(&backend.capabilities());
    let files = backend
        .list()
        .await
        .map_err(|error| error.to_string())?
        .len();
    let message = match &degraded {
        Some(note) => format!("{note}；远端已有 {files} 个笔记文件"),
        None => format!("连接成功，远端已有 {files} 个笔记文件"),
    };

    Ok(SyncTestResult {
        files,
        degraded,
        message,
    })
}

#[tauri::command]
pub fn sync_now(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let sync = state
        .settings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .sync
        .clone();
    if !sync.is_configured() {
        return Err("请先在设置里填写 WebDAV 地址和用户名".to_string());
    }
    if !sync.enabled {
        return Err("云同步已关闭".to_string());
    }
    sync::runtime::trigger(&app);
    Ok(())
}

/// 更新程序重启前调用：把还没落盘的设置同步写掉。
#[tauri::command]
pub fn flush_sync_state(app: AppHandle) -> Result<(), String> {
    persist_sync(&app)?;
    Ok(())
}

#[tauri::command]
pub fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("flashnote-commands-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sync_input(url: &str, enabled: bool) -> SyncConfigInput {
        SyncConfigInput {
            enabled,
            url: url.to_string(),
            username: " ranger ".to_string(),
            interval_minutes: 7,
            allow_insecure_http: false,
            device_name: " home-pc ".to_string(),
        }
    }

    #[test]
    fn sync_config_rejects_plain_http_without_permission() {
        let error = sync_input("http://dav.example.com/", true)
            .normalize()
            .unwrap_err();
        assert!(error.contains("http"), "{error}");

        let allowed = sync_input("http://127.0.0.1:5000/", true)
            .normalize()
            .unwrap();
        assert!(allowed.enabled);
    }

    #[test]
    fn sync_config_trims_and_normalizes() {
        let sync = sync_input("https://dav.example.com", true)
            .normalize()
            .unwrap();
        assert_eq!(sync.username, "ranger");
        assert_eq!(sync.device_name, "home-pc");
        assert_eq!(sync.interval_minutes, 15, "未知间隔回落到 15");
    }

    #[test]
    fn enabling_requires_url_and_username() {
        assert!(sync_input("", true).normalize().is_err());
        assert!(sync_input("", false).normalize().is_ok(), "关闭时允许留空");
    }

    #[test]
    fn sync_identity_ignores_trailing_slash_but_not_host_or_user() {
        let base = settings::SyncSettings {
            url: "https://dav.example.com/dav/".to_string(),
            username: "alice".to_string(),
            ..settings::SyncSettings::default()
        };
        let same = settings::SyncSettings {
            url: " https://dav.example.com/dav ".to_string(),
            username: " alice ".to_string(),
            ..settings::SyncSettings::default()
        };
        assert!(same_sync_identity(&base, &same));

        let other_host = settings::SyncSettings {
            url: "https://other.example.com/dav/".to_string(),
            ..base.clone()
        };
        assert!(!same_sync_identity(&base, &other_host));

        let other_user = settings::SyncSettings {
            username: "bob".to_string(),
            ..base.clone()
        };
        assert!(!same_sync_identity(&base, &other_user));
    }

    #[test]
    fn clamps_list_limit() {
        assert_eq!(clamp_limit(None), 200);
        assert_eq!(clamp_limit(Some(0)), 1, "0 条应当被抬到 1");
        assert_eq!(clamp_limit(Some(1)), 1);
        assert_eq!(clamp_limit(Some(1000)), 1000);
        assert_eq!(clamp_limit(Some(100_000)), 1000, "上限 1000");
    }

    #[test]
    fn settings_writes_keep_newest_successful_snapshot() {
        let dir = temp_dir("newest");
        let path = dir.join("settings.json");
        let last = AtomicU64::new(0);
        let lock = Mutex::new(());
        let older = Settings::default();
        let newer = Settings {
            pinned: true,
            ..Settings::default()
        };

        assert!(write_snapshot_locked(&path, 2, &newer, &last, &lock));
        assert!(
            !write_snapshot_locked(&path, 1, &older, &last, &lock),
            "已有更新的快照落盘，旧快照应跳过"
        );

        let saved: Settings = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(saved.pinned);
        assert_eq!(last.load(Ordering::SeqCst), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_write_does_not_block_older_snapshot() {
        let dir = temp_dir("failure");
        // 父路径是文件时 create_dir_all 必然失败，用来越过 settings::save 的错误分支
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, "x").unwrap();
        let bad_path = blocker.join("settings.json");
        let good_path = dir.join("settings.json");
        let last = AtomicU64::new(0);
        let lock = Mutex::new(());
        let snapshot = Settings {
            pinned: true,
            ..Settings::default()
        };

        assert!(!write_snapshot_locked(
            &bad_path, 2, &snapshot, &last, &lock
        ));
        assert_eq!(last.load(Ordering::SeqCst), 0, "失败不能推进已落盘序号");
        assert!(
            write_snapshot_locked(&good_path, 1, &snapshot, &last, &lock),
            "最新快照写失败时，旧快照仍应兜底"
        );
        assert!(good_path.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
