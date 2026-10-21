#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod notes;
mod settings;
mod source;
mod tray;

use std::sync::Mutex;
use tauri::Manager;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

/// 主快捷键被占用时依次尝试的备用组合。
const HOTKEY_FALLBACKS: [&str; 2] = ["ctrl+alt+m", "ctrl+shift+alt+n"];

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupNotice {
    pub message: String,
    pub open_settings: bool,
}

pub struct AppState {
    pub settings: Mutex<settings::Settings>,
    pub settings_path: std::path::PathBuf,
    pub last_source: Mutex<Option<String>>,
    startup_notice: Mutex<Option<StartupNotice>>,
}

fn register_hotkey(app: &tauri::AppHandle, accel: &str) -> Result<(), String> {
    let shortcut = accel
        .parse::<Shortcut>()
        .map_err(|_| "快捷键格式无效".to_string())?;
    app.global_shortcut()
        .register(shortcut)
        .map_err(|err| err.to_string())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            commands::show_window_impl(app);
        }))
        .plugin(
            tauri_plugin_log::Builder::new()
                .targets([
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::LogDir {
                        file_name: Some("flashnote".to_string()),
                    }),
                ])
                .level(log::LevelFilter::Info)
                .max_file_size(2 * 1024 * 1024)
                .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepSome(3))
                .build(),
        )
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
            let mut settings = loaded.settings;
            let mut notices = Vec::new();
            let mut open_settings = false;

            if let Some(warning) = loaded.warning {
                notices.push(warning);
            }

            log::info!(
                "闪记 v{} 启动，笔记目录：{}",
                env!("CARGO_PKG_VERSION"),
                settings.notes_dir
            );

            if let Err(err) = register_hotkey(&handle, &settings.hotkey) {
                log::warn!("注册全局热键 {} 失败：{err}", settings.hotkey);
                let failed = settings.hotkey.clone();
                let fallback = HOTKEY_FALLBACKS.iter().copied().find(|candidate| {
                    !candidate.eq_ignore_ascii_case(&failed)
                        && register_hotkey(&handle, candidate).is_ok()
                });

                match fallback {
                    Some(candidate) => {
                        log::warn!("已改用备用快捷键 {candidate}");
                        settings.hotkey = candidate.to_string();
                        if let Err(save_err) = settings::save(&settings_path, &settings) {
                            log::error!("保存备用快捷键失败：{save_err}");
                        }
                        notices.push(format!(
                            "快捷键 {failed} 被占用，已临时改用 {candidate}，可在设置中修改"
                        ));
                    }
                    None => {
                        log::error!("所有备用快捷键均注册失败");
                        notices.push(format!(
                            "快捷键 {failed} 注册失败（可能被其他程序占用），请在设置中更换"
                        ));
                    }
                }
                open_settings = true;
            } else {
                log::info!("全局热键已注册：{}", settings.hotkey);
            }

            let startup_notice = (!notices.is_empty()).then(|| StartupNotice {
                message: notices.join("；"),
                open_settings,
            });

            handle.manage(AppState {
                settings: Mutex::new(settings),
                settings_path,
                last_source: Mutex::new(None),
                startup_notice: Mutex::new(startup_notice),
            });

            commands::apply_initial_geometry(&handle);
            tray::create(app)?;

            if open_settings {
                commands::show_window_impl(&handle);
            }

            // 仅供开发时截图 / 调试使用
            if std::env::var("FLASHNOTE_SHOW_ON_START").as_deref() == Ok("1") {
                commands::show_window_impl(&handle);
            }

            Ok(())
        })
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                commands::hide_window_impl(window.app_handle());
            }
            tauri::WindowEvent::ScaleFactorChanged { .. } => {
                commands::reclamp_window(window.app_handle());
            }
            _ => {}
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
            commands::take_startup_notice,
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
