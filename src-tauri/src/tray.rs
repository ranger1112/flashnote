use crate::commands;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    App, Emitter,
};

pub fn create(app: &mut App) -> tauri::Result<()> {
    let show_item = MenuItem::with_id(app, "show", "显示速记", true, None::<&str>)?;
    let today_item = MenuItem::with_id(app, "today", "打开今日笔记", true, None::<&str>)?;
    let folder_item = MenuItem::with_id(app, "folder", "打开笔记文件夹", true, None::<&str>)?;
    let settings_item = MenuItem::with_id(app, "settings", "设置…", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &show_item,
            &today_item,
            &folder_item,
            &settings_item,
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
            "today" => {
                if let Err(err) = commands::open_today_note_impl(app) {
                    eprintln!("打开今日笔记失败: {err}");
                }
            }
            "folder" => {
                if let Err(err) = commands::open_notes_dir_impl(app) {
                    eprintln!("打开笔记目录失败: {err}");
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
