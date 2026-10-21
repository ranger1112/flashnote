use crate::{
    notes,
    settings::{self, Settings, WindowGeometry},
    source, AppState,
};
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, State, WebviewWindow,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

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

pub fn persist(app: &AppHandle) {
    let state = app.state::<AppState>();
    let snapshot = state.settings.lock().unwrap().clone();
    let _ = settings::save(&state.settings_path, &snapshot);
}

fn persist_geometry(app: &AppHandle, window: &WebviewWindow) {
    if let (Ok(pos), Ok(size)) = (window.outer_position(), window.inner_size()) {
        if let Ok(mut settings) = app.state::<AppState>().settings.lock() {
            settings.window = Some(WindowGeometry {
                x: pos.x,
                y: pos.y,
                w: size.width,
                h: size.height,
            });
        }
    }
}

fn top_right_position(window: &WebviewWindow) -> Option<(i32, i32)> {
    let monitor = window.primary_monitor().ok().flatten()?;
    let pos = monitor.position();
    let size = monitor.size();
    let window_size = window
        .inner_size()
        .unwrap_or(PhysicalSize::new(480, 360));
    let margin = (24.0 * monitor.scale_factor()) as i32;
    Some((
        pos.x + size.width as i32 - window_size.width as i32 - margin,
        pos.y + margin,
    ))
}

fn clamp_position(window: &WebviewWindow, x: i32, y: i32) -> (i32, i32) {
    if let Ok(monitors) = window.available_monitors() {
        for monitor in &monitors {
            let pos = monitor.position();
            let size = monitor.size();
            let inside = x >= pos.x - 40
                && x < pos.x + size.width as i32 - 80
                && y >= pos.y - 40
                && y < pos.y + size.height as i32 - 80;
            if inside {
                return (x, y);
            }
        }
    }
    top_right_position(window).unwrap_or((x, y))
}

pub fn apply_initial_geometry(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let saved = app
        .state::<AppState>()
        .settings
        .lock()
        .unwrap()
        .window
        .clone();

    match saved {
        Some(geometry) => {
            let (x, y) = clamp_position(&window, geometry.x, geometry.y);
            let _ = window.set_position(PhysicalPosition::new(x, y));
            let _ = window.set_size(PhysicalSize::new(
                geometry.w.max(380),
                geometry.h.max(240),
            ));
        }
        None => {
            if let Some((x, y)) = top_right_position(&window) {
                let _ = window.set_position(PhysicalPosition::new(x, y));
            }
        }
    }
}

pub fn show_window_impl(app: &AppHandle) {
    let state = app.state::<AppState>();
    let capture = state.settings.lock().unwrap().capture_source;

    let source = if capture {
        match source::foreground_window_title() {
            Some(title) => {
                *state.last_source.lock().unwrap() = Some(title.clone());
                Some(title)
            }
            // 托盘菜单会短暂抢走前台窗口，抓不到时沿用上一次的来源
            None => state.last_source.lock().unwrap().clone(),
        }
    } else {
        None
    };

    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        let _ = app.emit("window-shown", ShowPayload { source });
    }
}

pub fn hide_window_impl(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        persist_geometry(app, &window);
        persist(app);
        let _ = window.hide();
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

fn open_with_explorer(path: &Path) -> Result<(), String> {
    std::process::Command::new("explorer")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|err| err.to_string())
}

pub fn open_today_note_impl(app: &AppHandle) -> Result<(), String> {
    let notes_dir = app.state::<AppState>().settings.lock().unwrap().notes_dir.clone();
    let (date, _) = notes::now_parts();
    let dir = PathBuf::from(notes_dir);
    std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    let path = dir.join(format!("{date}.md"));
    if !path.exists() {
        std::fs::write(&path, format!("# {date}\n")).map_err(|err| err.to_string())?;
    }
    open_with_explorer(&path)
}

pub fn open_notes_dir_impl(app: &AppHandle) -> Result<(), String> {
    let notes_dir = app.state::<AppState>().settings.lock().unwrap().notes_dir.clone();
    let dir = PathBuf::from(notes_dir);
    std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    open_with_explorer(&dir)
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
pub fn save_note(
    _app: AppHandle,
    state: State<'_, AppState>,
    content: String,
) -> Result<SavedNote, String> {
    let content = content.trim();
    if content.is_empty() {
        return Err("内容为空".to_string());
    }

    let (settings, source_title) = {
        let settings = state.settings.lock().unwrap().clone();
        let source_title = if settings.capture_source {
            state.last_source.lock().unwrap().clone()
        } else {
            None
        };
        (settings, source_title)
    };

    let (date, time) = notes::now_parts();
    let entry = notes::format_entry(&time, content, source_title.as_deref());
    let dir = PathBuf::from(&settings.notes_dir);
    let path = notes::append_note(&dir, &date, &entry)
        .map_err(|err| format!("写入失败：{err}"))?;

    Ok(SavedNote {
        path: path.to_string_lossy().to_string(),
        time,
    })
}

#[tauri::command]
pub fn set_pinned(app: AppHandle, state: State<'_, AppState>, pinned: bool) -> Result<(), String> {
    state.settings.lock().unwrap().pinned = pinned;
    persist(&app);
    Ok(())
}

#[tauri::command]
pub fn update_notes_dir(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> Result<(), String> {
    let dir = PathBuf::from(path.trim());
    if dir.as_os_str().is_empty() {
        return Err("目录不能为空".to_string());
    }
    std::fs::create_dir_all(&dir).map_err(|err| format!("无法创建目录：{err}"))?;
    state.settings.lock().unwrap().notes_dir = dir.to_string_lossy().to_string();
    persist(&app);
    Ok(())
}

#[tauri::command]
pub fn update_hotkey(
    app: AppHandle,
    state: State<'_, AppState>,
    accel: String,
) -> Result<(), String> {
    let shortcut: Shortcut = accel
        .parse()
        .map_err(|_| "快捷键格式无效".to_string())?;

    let old = state.settings.lock().unwrap().hotkey.clone();
    let _ = app.global_shortcut().unregister_all();

    match app.global_shortcut().register(shortcut) {
        Ok(()) => {
            state.settings.lock().unwrap().hotkey = accel;
            persist(&app);
            Ok(())
        }
        Err(err) => {
            if let Ok(previous) = old.parse::<Shortcut>() {
                let _ = app.global_shortcut().register(previous);
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
    state.settings.lock().unwrap().capture_source = enabled;
    if !enabled {
        *state.last_source.lock().unwrap() = None;
    }
    persist(&app);
    Ok(())
}

#[tauri::command]
pub fn open_today_note(app: AppHandle) -> Result<(), String> {
    open_today_note_impl(&app)
}

#[tauri::command]
pub fn open_notes_dir(app: AppHandle) -> Result<(), String> {
    open_notes_dir_impl(&app)
}

#[tauri::command]
pub fn hide_window(app: AppHandle) {
    hide_window_impl(&app);
}
