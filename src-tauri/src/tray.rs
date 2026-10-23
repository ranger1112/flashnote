use crate::{commands, files, AppState};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    App, AppHandle, Emitter, Manager,
};

/// 托盘回调跑在 UI 线程上；打开目录/文件可能落在掉线的网络盘上，
/// 丢进阻塞线程池再执行，菜单点击后窗口不会跟着卡住。
fn run_off_thread<F>(app: &AppHandle, task: F, context: &'static str)
where
    F: FnOnce(&AppHandle) -> Result<(), String> + Send + 'static,
{
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        if let Err(err) = task(&handle) {
            log::error!("{context}：{err}");
        }
    });
}

pub fn create(app: &mut App) -> tauri::Result<()> {
    let show_item = MenuItem::with_id(app, "show", "显示速记", true, None::<&str>)?;
    let history_item = MenuItem::with_id(app, "history", "查看笔记", true, None::<&str>)?;
    let today_item = MenuItem::with_id(app, "today", "打开今日笔记", true, None::<&str>)?;
    let folder_item = MenuItem::with_id(app, "folder", "打开笔记文件夹", true, None::<&str>)?;
    let sync_item = MenuItem::with_id(app, "sync", "立即同步", true, None::<&str>)?;
    let settings_item = MenuItem::with_id(app, "settings", "设置…", true, None::<&str>)?;
    let logs_item = MenuItem::with_id(app, "logs", "打开日志文件夹", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &show_item,
            &history_item,
            &today_item,
            &folder_item,
            &sync_item,
            &settings_item,
            &logs_item,
            &quit_item,
        ],
    )?;

    TrayIconBuilder::new()
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("闪记 FlashNote")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => commands::show_window_impl(app),
            "history" => {
                commands::show_window_impl(app);
                let _ = app.emit("open-history", ());
            }
            "today" => run_off_thread(app, files::open_today_note_impl, "打开今日笔记失败"),
            "folder" => run_off_thread(app, files::open_notes_dir_impl, "打开笔记目录失败"),
            "logs" => run_off_thread(app, files::open_log_dir_impl, "打开日志目录失败"),
            "sync" => {
                let sync = app
                    .state::<AppState>()
                    .settings
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .sync
                    .clone();
                if sync.enabled && sync.is_configured() {
                    // trigger 只起一个后台任务，菜单线程立刻返回。
                    crate::sync::runtime::trigger(app);
                } else {
                    // 没配置好就去设置页，而不是静默失败。
                    commands::show_window_impl(app);
                    let _ = app.emit("open-settings", ());
                }
            }
            "settings" => {
                commands::show_window_impl(app);
                let _ = app.emit("open-settings", ());
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                commands::show_window_impl(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(())
}
