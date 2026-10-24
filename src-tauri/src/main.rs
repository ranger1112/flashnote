#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod files;
mod geometry;
mod notes;
mod settings;
mod source;
mod sync;
mod tray;

use std::sync::{atomic::AtomicU64, Arc, Mutex};
use tauri::Manager;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

/// 解析快捷键，并拒绝没有修饰键的组合——裸键注册成全局热键会吞掉所有输入。
pub(crate) fn parse_hotkey(accel: &str) -> Result<Shortcut, String> {
    let shortcut: Shortcut = accel.parse().map_err(|_| "快捷键格式无效".to_string())?;
    if shortcut.mods.is_empty() {
        return Err("快捷键至少需要一个修饰键（Ctrl / Alt / Shift / Win）".to_string());
    }
    Ok(shortcut)
}

/// release 构建没有控制台，启动失败时用 MessageBox 把原因显示出来。
#[cfg(windows)]
fn fatal_dialog(message: &str) {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

    fn wide(value: &str) -> Vec<u16> {
        OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    let text = wide(message);
    let title = wide("闪记启动失败");
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(not(windows))]
fn fatal_dialog(message: &str) {
    eprintln!("{message}");
}

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
    /// 串行化笔记文件读写：命令改成 async 后可能真正并发，追加与改写不能交叉。
    /// 用 `Arc` 是为了让同步引擎共用同一把锁。
    pub io: Arc<Mutex<()>>,
    /// 云同步运行时：调度、单飞与状态。
    pub(crate) sync: Arc<sync::runtime::SyncRuntime>,
    /// 设置写入序号：每次持久化分配一个递增序号。
    pub settings_write_seq: AtomicU64,
    /// 最近一次真正写成功的序号；旧快照据此跳过，写入失败时不会误伤兜底。
    pub settings_last_saved_seq: AtomicU64,
    /// 串行化设置文件写入，避免两个快照同时写同一个临时文件。
    pub settings_write_lock: Mutex<()>,
    startup_notice: Mutex<Option<StartupNotice>>,
}

fn register_hotkey(app: &tauri::AppHandle, accel: &str) -> Result<(), String> {
    let shortcut = parse_hotkey(accel)?;
    app.global_shortcut()
        .register(shortcut)
        .map_err(|err| err.to_string())
}

fn main() {
    let app = tauri::Builder::default()
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
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
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
            let first_run = !settings.onboarded;

            handle.manage(AppState {
                settings: Mutex::new(settings),
                settings_path,
                last_source: Mutex::new(None),
                io: Arc::new(Mutex::new(())),
                sync: Arc::new(sync::runtime::SyncRuntime::new()),
                settings_write_seq: AtomicU64::new(0),
                settings_last_saved_seq: AtomicU64::new(0),
                settings_write_lock: Mutex::new(()),
                startup_notice: Mutex::new(startup_notice),
            });

            geometry::apply_initial_geometry(&handle);
            tray::create(app)?;
            sync::runtime::start(&handle);

            if open_settings || first_run {
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
                geometry::reclamp_window(window.app_handle());
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
            commands::search_notes,
            commands::delete_note_entry,
            commands::update_note_entry,
            commands::open_note_file,
            commands::open_today_note,
            commands::open_notes_dir,
            commands::open_external_url,
            commands::hide_window,
            commands::take_startup_notice,
            commands::load_draft,
            commands::save_draft,
            commands::clear_draft,
            commands::complete_onboarding,
            commands::update_history_preview,
            commands::update_auto_update,
            commands::update_theme,
            commands::update_auto_hide,
            commands::update_background_opacity,
            commands::get_sync_status,
            commands::update_sync_config,
            commands::save_sync_password,
            commands::clear_sync_password,
            commands::test_sync_connection,
            commands::sync_now,
            commands::flush_sync_state,
            commands::app_version,
        ])
        .build(tauri::generate_context!());

    let app = match app {
        Ok(app) => app,
        Err(err) => {
            log::error!("启动失败：{err}");
            fatal_dialog(&format!("闪记启动失败：\n{err}"));
            std::process::exit(1);
        }
    };

    app.run(|app, event| {
        if matches!(
            event,
            tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
        ) {
            commands::persist_on_exit(app);
        }
    });
}
