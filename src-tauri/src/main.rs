#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod notes;
mod settings;
mod source;
mod tray;

use std::sync::Mutex;
use tauri::Manager;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

pub struct AppState {
    pub settings: Mutex<settings::Settings>,
    pub settings_path: std::path::PathBuf,
    pub last_source: Mutex<Option<String>>,
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            commands::show_window_impl(app);
        }))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        commands::toggle_window(app);
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None::<Vec<&str>>,
        ))
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let loaded = settings::load(&handle);
            let settings_path = settings::settings_path(&handle);

            handle.manage(AppState {
                settings: Mutex::new(loaded.clone()),
                settings_path,
                last_source: Mutex::new(None),
            });

            if let Ok(shortcut) = loaded.hotkey.parse::<Shortcut>() {
                if let Err(err) = handle.global_shortcut().register(shortcut) {
                    eprintln!("注册全局热键 {} 失败: {err}", loaded.hotkey);
                }
            }

            commands::apply_initial_geometry(&handle);
            tray::create(app)?;

            // 仅供开发时截图 / 调试使用
            if std::env::var("FLASHNOTE_SHOW_ON_START").as_deref() == Ok("1") {
                commands::show_window_impl(&handle);
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                commands::hide_window_impl(window.app_handle());
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::save_note,
            commands::set_pinned,
            commands::update_notes_dir,
            commands::update_hotkey,
            commands::update_capture_source,
            commands::update_markdown_mode,
            commands::list_notes,
            commands::open_note_file,
            commands::open_today_note,
            commands::open_notes_dir,
            commands::hide_window,
        ])
        .build(tauri::generate_context!())
        .expect("启动闪记失败")
        .run(|app, event| {
            if matches!(
                event,
                tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
            ) {
                commands::persist(app);
            }
        });
}
