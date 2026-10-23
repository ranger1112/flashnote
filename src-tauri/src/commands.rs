//! 前后端命令：设置、窗口显隐、笔记读写与草稿。
//!
//! 文件 I/O 一律通过 `run_blocking` 放进阻塞线程池，设置持久化则交给
//! 非阻塞的 `persist`：UI 线程与 tokio worker 都不该等磁盘。

use crate::{
    files::{self, io_guard},
    notes,
    settings::{self, Settings},
    AppState,
};
use serde::Serialize;
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
pub fn persist_sync(app: &AppHandle) {
    let state = app.state::<AppState>();
    let (snapshot, seq) = {
        let settings = state
            .settings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let seq = state.settings_write_seq.fetch_add(1, Ordering::SeqCst) + 1;
        (settings.clone(), seq)
    };
    write_snapshot(&state, &state.settings_path, seq, &snapshot);
}

fn write_snapshot(state: &AppState, path: &Path, seq: u64, snapshot: &Settings) {
    write_snapshot_locked(
        path,
        seq,
        snapshot,
        &state.settings_last_saved_seq,
        &state.settings_write_lock,
    );
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
    persist_sync(app);
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
    let dir = {
        let handle = app.clone();
        run_blocking(move || {
            let state = handle.state::<AppState>();
            // 网络目录掉线时 create_dir_all 会阻塞很久，别让并发的笔记读写跟着卡住
            let _guard = io_guard(&state);
            files::ensure_writable_dir(&path)
        })
        .await?
    };
    {
        let state = app.state::<AppState>();
        state
            .settings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .notes_dir = dir.to_string_lossy().to_string();
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
        files::rewrite_note(&state, &path, |text| {
            notes::remove_entry(text, &date, &time, &content)
        })
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
        files::rewrite_note(&state, &path, |text| {
            notes::replace_entry(text, &date, &time, &content, &new_content)
        })
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
