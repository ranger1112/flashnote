use crate::{
    notes,
    settings::{self, Settings, WindowGeometry},
    source, AppState,
};
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, State, WebviewWindow};
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
    if let Err(err) = settings::save(&state.settings_path, &snapshot) {
        log::error!("保存设置失败：{err}");
    }
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
    let area = monitor.work_area();
    let window_size = window.inner_size().unwrap_or(PhysicalSize::new(480, 360));
    let margin = (24.0 * monitor.scale_factor()) as i32;
    Some((
        area.position.x + area.size.width as i32 - window_size.width as i32 - margin,
        area.position.y + margin,
    ))
}

/// 物理像素矩形，用于窗口与显示器工作区之间的夹取计算。
#[derive(Clone, Copy, Debug)]
pub(crate) struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    fn right(self) -> i32 {
        self.x.saturating_add(self.w as i32)
    }

    fn bottom(self) -> i32 {
        self.y.saturating_add(self.h as i32)
    }
}

fn overlap_area(a: Rect, b: Rect) -> i64 {
    let left = a.x.max(b.x) as i64;
    let top = a.y.max(b.y) as i64;
    let right = a.right().min(b.right()) as i64;
    let bottom = a.bottom().min(b.bottom()) as i64;
    if right <= left || bottom <= top {
        0
    } else {
        (right - left) * (bottom - top)
    }
}

fn monitor_rects(window: &WebviewWindow) -> Vec<Rect> {
    window
        .available_monitors()
        .map(|monitors| {
            monitors
                .iter()
                .map(|monitor| {
                    let area = monitor.work_area();
                    Rect {
                        x: area.position.x,
                        y: area.position.y,
                        w: area.size.width,
                        h: area.size.height,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 把窗口位置夹回某块显示器的工作区：优先选择与保存位置重叠面积最大的屏幕，
/// 完全不相交时退到中心点最近的一块。
pub(crate) fn clamp_rect(rect: Rect, monitors: &[Rect]) -> (i32, i32) {
    let Some(first) = monitors.first() else {
        return (rect.x, rect.y);
    };

    let mut best = *first;
    let mut best_area = overlap_area(rect, best);
    for monitor in monitors.iter().skip(1) {
        let area = overlap_area(rect, *monitor);
        if area > best_area {
            best = *monitor;
            best_area = area;
        }
    }

    if best_area == 0 {
        let center_x = rect.x.saturating_add(rect.w as i32 / 2);
        let center_y = rect.y.saturating_add(rect.h as i32 / 2);
        best = *monitors
            .iter()
            .min_by_key(|monitor| {
                let dx = monitor.x.saturating_add(monitor.w as i32 / 2) - center_x;
                let dy = monitor.y.saturating_add(monitor.h as i32 / 2) - center_y;
                dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy))
            })
            .unwrap_or(first);
    }

    // 至少保留左侧 120px、顶部 40px（标题栏）可见，保证窗口可以被拖回来
    let visible_x = 120.min(rect.w as i32);
    let visible_y = 40.min(rect.h as i32);
    let min_x = best.x - (rect.w as i32 - visible_x);
    let max_x = best.x + best.w as i32 - visible_x;
    let min_y = best.y;
    let max_y = best.y + best.h as i32 - visible_y;

    let x = if min_x <= max_x {
        rect.x.clamp(min_x, max_x)
    } else {
        best.x
    };
    let y = if min_y <= max_y {
        rect.y.clamp(min_y, max_y)
    } else {
        best.y
    };

    (x, y)
}

/// 尺寸限制在最小窗口尺寸与所在显示器工作区之间。
pub(crate) fn clamp_size(w: u32, h: u32, x: i32, y: i32, monitors: &[Rect]) -> (u32, u32) {
    const MIN_W: u32 = 380;
    const MIN_H: u32 = 240;

    let monitor = monitors
        .iter()
        .find(|monitor| {
            x >= monitor.x
                && x < monitor.x.saturating_add(monitor.w as i32)
                && y >= monitor.y
                && y < monitor.y.saturating_add(monitor.h as i32)
        })
        .or_else(|| monitors.first());

    match monitor {
        Some(monitor) => (
            w.clamp(MIN_W, monitor.w.max(MIN_W)),
            h.clamp(MIN_H, monitor.h.max(MIN_H)),
        ),
        None => (w.max(MIN_W), h.max(MIN_H)),
    }
}

fn clamp_position(window: &WebviewWindow, x: i32, y: i32) -> (i32, i32) {
    let monitors = monitor_rects(window);
    if monitors.is_empty() {
        return top_right_position(window).unwrap_or((x, y));
    }
    let size = window.inner_size().unwrap_or(PhysicalSize::new(480, 360));
    clamp_rect(
        Rect {
            x,
            y,
            w: size.width,
            h: size.height,
        },
        &monitors,
    )
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
            let monitors = monitor_rects(&window);
            let (x, y) = clamp_rect(
                Rect {
                    x: geometry.x,
                    y: geometry.y,
                    w: geometry.w,
                    h: geometry.h,
                },
                &monitors,
            );
            let (w, h) = clamp_size(geometry.w, geometry.h, x, y, &monitors);
            if let Err(err) = window.set_position(PhysicalPosition::new(x, y)) {
                log::warn!("恢复窗口位置失败：{err}");
            }
            if let Err(err) = window.set_size(PhysicalSize::new(w, h)) {
                log::warn!("恢复窗口尺寸失败：{err}");
            }
        }
        None => {
            if let Some((x, y)) = top_right_position(&window) {
                if let Err(err) = window.set_position(PhysicalPosition::new(x, y)) {
                    log::warn!("设置窗口初始位置失败：{err}");
                }
            }
        }
    }
}

/// 系统缩放比例变化（切换显示器、修改缩放）后重新夹取窗口位置。
pub fn reclamp_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let Ok(position) = window.outer_position() else {
        return;
    };
    let (x, y) = clamp_position(&window, position.x, position.y);
    if (x, y) != (position.x, position.y) {
        if let Err(err) = window.set_position(PhysicalPosition::new(x, y)) {
            log::warn!("缩放变化后调整窗口位置失败：{err}");
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
        persist_geometry(app, &window);
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

fn open_with_explorer(path: &Path) -> Result<(), String> {
    std::process::Command::new("explorer")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|err| err.to_string())
}

/// 交给系统默认关联程序打开（Markdown 文件通常是编辑器）。
fn open_with_default_app(path: &Path) -> Result<(), String> {
    std::process::Command::new("cmd")
        .args(["/C", "start", "", path.to_string_lossy().as_ref()])
        .spawn()
        .map(|_| ())
        .map_err(|err| err.to_string())
}

pub fn open_today_note_impl(app: &AppHandle) -> Result<(), String> {
    let notes_dir = app
        .state::<AppState>()
        .settings
        .lock()
        .unwrap()
        .notes_dir
        .clone();
    let (date, _) = notes::now_parts();
    let dir = PathBuf::from(notes_dir);
    std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    let path = dir.join(format!("{date}.md"));
    if !path.exists() {
        std::fs::write(&path, format!("# {date}\n")).map_err(|err| err.to_string())?;
    }
    open_with_default_app(&path)
}

pub fn open_notes_dir_impl(app: &AppHandle) -> Result<(), String> {
    let notes_dir = app
        .state::<AppState>()
        .settings
        .lock()
        .unwrap()
        .notes_dir
        .clone();
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
    let entry = if settings.markdown_mode {
        notes::format_entry_markdown(&time, content, source_title.as_deref())
    } else {
        notes::format_entry(&time, content, source_title.as_deref())
    };
    let dir = PathBuf::from(&settings.notes_dir);
    let path = notes::append_note(&dir, &date, &entry).map_err(|err| format!("写入失败：{err}"))?;

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

/// 校验目录存在且可写，返回创建好的路径。
pub(crate) fn ensure_writable_dir(path: &str) -> Result<PathBuf, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("目录不能为空".to_string());
    }

    let dir = PathBuf::from(trimmed);
    if dir.exists() && !dir.is_dir() {
        return Err("所选路径不是文件夹".to_string());
    }
    std::fs::create_dir_all(&dir).map_err(|err| format!("无法创建目录：{err}"))?;

    let probe = dir.join(format!(".flashnote-write-test-{}", std::process::id()));
    std::fs::write(&probe, b"flashnote").map_err(|err| format!("该目录不可写：{err}"))?;
    if let Err(err) = std::fs::remove_file(&probe) {
        log::warn!("清理写入测试文件失败（{}）：{err}", probe.display());
    }

    Ok(dir)
}

#[tauri::command]
pub fn update_notes_dir(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> Result<(), String> {
    let dir = ensure_writable_dir(&path)?;
    state.settings.lock().unwrap().notes_dir = dir.to_string_lossy().to_string();
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
    let shortcut: Shortcut = accel.parse().map_err(|_| "快捷键格式无效".to_string())?;

    let old = state.settings.lock().unwrap().hotkey.clone();
    if let Err(err) = app.global_shortcut().unregister_all() {
        log::warn!("取消旧快捷键失败：{err}");
    }

    match app.global_shortcut().register(shortcut) {
        Ok(()) => {
            state.settings.lock().unwrap().hotkey = accel.clone();
            persist(&app);
            log::info!("全局热键已更新为 {accel}");
            Ok(())
        }
        Err(err) => {
            log::warn!("注册新快捷键 {accel} 失败：{err}");
            if let Ok(previous) = old.parse::<Shortcut>() {
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
    state.settings.lock().unwrap().capture_source = enabled;
    if !enabled {
        *state.last_source.lock().unwrap() = None;
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
    state.settings.lock().unwrap().markdown_mode = enabled;
    persist(&app);
    Ok(())
}

#[tauri::command]
pub fn list_notes(
    state: State<'_, AppState>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Vec<notes::NoteEntry>, String> {
    let dir = PathBuf::from(&state.settings.lock().unwrap().notes_dir);
    notes::load_recent(&dir, clamp_limit(limit), offset.unwrap_or(0) as usize)
        .map_err(|err| format!("读取笔记失败：{err}"))
}

#[tauri::command]
pub fn search_notes(
    state: State<'_, AppState>,
    query: String,
    limit: Option<u32>,
) -> Result<Vec<notes::NoteEntry>, String> {
    let dir = PathBuf::from(&state.settings.lock().unwrap().notes_dir);
    notes::search_entries(&dir, &query, clamp_limit(limit))
        .map_err(|err| format!("搜索笔记失败：{err}"))
}

/// 列表条数限制在 1..=1000，缺省 200。
fn clamp_limit(limit: Option<u32>) -> usize {
    limit.unwrap_or(200).clamp(1, 1000) as usize
}

/// 把目标文件解析到笔记目录内。`..` 穿越、指向目录外的绝对路径都会被拒绝。
fn resolve_within(dir: &Path, path: &str) -> Result<(PathBuf, PathBuf), String> {
    let dir = dir
        .canonicalize()
        .map_err(|err| format!("笔记目录不可用：{err}"))?;
    let target = PathBuf::from(path)
        .canonicalize()
        .map_err(|err| format!("文件不存在：{err}"))?;
    if !target.starts_with(&dir) {
        return Err("文件不在笔记目录中".to_string());
    }
    Ok((dir, target))
}

/// 在笔记目录内改写单个文件：先备份，再原子替换。
fn rewrite_note<F>(state: &AppState, path: &str, update: F) -> Result<(), String>
where
    F: FnOnce(&str) -> Option<String>,
{
    let notes_dir = PathBuf::from(&state.settings.lock().unwrap().notes_dir);
    let (dir, target) = resolve_within(&notes_dir, path)?;

    let text = std::fs::read_to_string(&target).map_err(|err| format!("读取失败：{err}"))?;
    let updated =
        update(&text).ok_or_else(|| "没有找到这条笔记，可能已被其他程序修改".to_string())?;

    let stem = target
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("notes");
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let backup = target.with_file_name(format!("{stem}.md.bak-{stamp}"));
    std::fs::copy(&target, &backup).map_err(|err| format!("备份失败：{err}"))?;

    let tmp = target.with_extension("md.tmp");
    std::fs::write(&tmp, updated.as_bytes()).map_err(|err| format!("写入失败：{err}"))?;
    std::fs::rename(&tmp, &target).map_err(|err| format!("写入失败：{err}"))?;

    prune_backups(&dir, stem);
    Ok(())
}

/// 每个日期文件最多保留 5 份备份，避免长期使用后目录膨胀。
fn prune_backups(dir: &Path, stem: &str) {
    let prefix = format!("{stem}.md.bak-");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut backups: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&prefix))
        })
        .collect();
    backups.sort();
    while backups.len() > 5 {
        let oldest = backups.remove(0);
        let _ = std::fs::remove_file(oldest);
    }
}

#[tauri::command]
pub fn delete_note_entry(
    state: State<'_, AppState>,
    path: String,
    date: String,
    time: String,
    content: String,
) -> Result<(), String> {
    rewrite_note(&state, &path, |text| {
        notes::remove_entry(text, &date, &time, &content)
    })
}

#[tauri::command]
pub fn update_note_entry(
    state: State<'_, AppState>,
    path: String,
    date: String,
    time: String,
    content: String,
    new_content: String,
) -> Result<(), String> {
    if new_content.trim().is_empty() {
        return Err("内容不能为空".to_string());
    }
    rewrite_note(&state, &path, |text| {
        notes::replace_entry(text, &date, &time, &content, &new_content)
    })
}

#[tauri::command]
pub fn open_note_file(state: State<'_, AppState>, path: String) -> Result<(), String> {
    let notes_dir = PathBuf::from(&state.settings.lock().unwrap().notes_dir);
    let (_, target) = resolve_within(&notes_dir, &path)?;
    open_with_explorer(&target)
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

#[tauri::command]
pub fn take_startup_notice(state: State<'_, AppState>) -> Option<crate::StartupNotice> {
    state.startup_notice.lock().unwrap().take()
}

pub fn open_log_dir_impl(app: &AppHandle) -> Result<(), String> {
    let dir = app.path().app_log_dir().map_err(|err| err.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    open_with_explorer(&dir)
}

fn draft_path(app: &AppHandle) -> PathBuf {
    settings::settings_path(app).with_file_name("draft.txt")
}

#[tauri::command]
pub fn load_draft(app: AppHandle) -> Result<String, String> {
    match std::fs::read_to_string(draft_path(&app)) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(format!("读取草稿失败：{err}")),
    }
}

#[tauri::command]
pub fn save_draft(app: AppHandle, content: String) -> Result<(), String> {
    let path = draft_path(&app);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| format!("保存草稿失败：{err}"))?;
    }
    let tmp = path.with_extension("txt.tmp");
    std::fs::write(&tmp, content.as_bytes()).map_err(|err| format!("保存草稿失败：{err}"))?;
    std::fs::rename(&tmp, &path).map_err(|err| format!("保存草稿失败：{err}"))
}

#[tauri::command]
pub fn clear_draft(app: AppHandle) -> Result<(), String> {
    match std::fs::remove_file(draft_path(&app)) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!("清除草稿失败：{err}")),
    }
}

#[tauri::command]
pub fn complete_onboarding(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    state.settings.lock().unwrap().onboarded = true;
    persist(&app);
    Ok(())
}

#[tauri::command]
pub fn update_history_preview(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<(), String> {
    state.settings.lock().unwrap().history_preview = enabled;
    persist(&app);
    Ok(())
}

#[tauri::command]
pub fn update_auto_update(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<(), String> {
    state.settings.lock().unwrap().auto_update = enabled;
    persist(&app);
    Ok(())
}

#[tauri::command]
pub fn update_theme(app: AppHandle, state: State<'_, AppState>, theme: String) -> String {
    let applied = settings::normalize_theme(&theme);
    state.settings.lock().unwrap().theme = applied.clone();
    persist(&app);
    applied
}

#[tauri::command]
pub fn update_auto_hide(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<(), String> {
    state.settings.lock().unwrap().auto_hide_on_blur = enabled;
    persist(&app);
    Ok(())
}

#[tauri::command]
pub fn update_background_opacity(
    app: AppHandle,
    state: State<'_, AppState>,
    opacity: f64,
) -> f64 {
    let applied = settings::clamp_opacity(opacity);
    state.settings.lock().unwrap().background_opacity = applied;
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
    fn clamp_rect_keeps_window_in_place_when_visible() {
        let monitors = [
            Rect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1040,
            },
            Rect {
                x: 1920,
                y: 0,
                w: 1920,
                h: 1040,
            },
        ];

        assert_eq!(
            clamp_rect(
                Rect {
                    x: 100,
                    y: 100,
                    w: 480,
                    h: 360
                },
                &monitors
            ),
            (100, 100)
        );
        assert_eq!(
            clamp_rect(
                Rect {
                    x: 2000,
                    y: 200,
                    w: 480,
                    h: 360
                },
                &monitors
            ),
            (2000, 200)
        );
    }

    #[test]
    fn clamp_rect_pulls_back_offscreen_window() {
        let monitors = [Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1040,
        }];

        let (x, y) = clamp_rect(
            Rect {
                x: 5000,
                y: 5000,
                w: 480,
                h: 360,
            },
            &monitors,
        );

        assert_eq!(x, 1920 - 120);
        assert_eq!(y, 1040 - 40);
    }

    #[test]
    fn clamp_rect_falls_back_to_nearest_monitor_after_unplug() {
        let monitors = [Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1040,
        }];

        let (x, y) = clamp_rect(
            Rect {
                x: 2500,
                y: 100,
                w: 480,
                h: 360,
            },
            &monitors,
        );

        assert!(x <= 1920 - 120);
        assert!(y >= 0);
    }

    #[test]
    fn clamp_rect_without_monitors_is_a_no_op() {
        let rect = Rect {
            x: 123,
            y: 456,
            w: 480,
            h: 360,
        };
        assert_eq!(clamp_rect(rect, &[]), (123, 456));
    }

    #[test]
    fn clamp_size_respects_minimum_and_monitor_bounds() {
        let monitors = [Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1040,
        }];

        assert_eq!(clamp_size(100, 100, 10, 10, &monitors), (380, 240));
        assert_eq!(clamp_size(4000, 3000, 10, 10, &monitors), (1920, 1040));
    }

    #[test]
    fn ensure_writable_dir_creates_and_accepts_directory() {
        let dir = temp_dir("writable");
        let target = dir.join("notes");

        let result = ensure_writable_dir(target.to_str().unwrap()).unwrap();

        assert!(result.is_dir());
        assert!(std::fs::read_dir(&result).unwrap().next().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_writable_dir_rejects_file_path() {
        let dir = temp_dir("file-path");
        let file = dir.join("not-a-dir.txt");
        std::fs::write(&file, "x").unwrap();

        assert!(ensure_writable_dir(file.to_str().unwrap()).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_writable_dir_rejects_empty_path() {
        assert!(ensure_writable_dir("   ").is_err());
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
    fn resolve_within_accepts_file_in_notes_dir() {
        let dir = temp_dir("resolve-inside");
        let file = dir.join("2026-10-21.md");
        std::fs::write(&file, "# 2026-10-21\n").unwrap();

        let (_, target) = resolve_within(&dir, file.to_str().unwrap()).unwrap();
        assert_eq!(target, file.canonicalize().unwrap());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_within_rejects_parent_traversal() {
        let dir = temp_dir("resolve-traversal");
        let notes = dir.join("notes");
        std::fs::create_dir_all(&notes).unwrap();
        let outside = dir.join("secret.md");
        std::fs::write(&outside, "x").unwrap();

        let sneaky = notes.join("..").join("secret.md");
        let err = resolve_within(&notes, sneaky.to_str().unwrap()).unwrap_err();
        assert_eq!(err, "文件不在笔记目录中");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_within_rejects_absolute_path_outside() {
        let dir = temp_dir("resolve-outside");
        let notes = dir.join("notes");
        std::fs::create_dir_all(&notes).unwrap();
        let outside = dir.join("elsewhere.md");
        std::fs::write(&outside, "x").unwrap();

        let err = resolve_within(&notes, outside.to_str().unwrap()).unwrap_err();
        assert_eq!(err, "文件不在笔记目录中");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_within_rejects_missing_file() {
        let dir = temp_dir("resolve-missing");
        let missing = dir.join("nope.md");

        let err = resolve_within(&dir, missing.to_str().unwrap()).unwrap_err();
        assert!(err.starts_with("文件不存在"), "实际：{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
