#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app_lock;
mod commands;
mod files;
mod geometry;
mod git_backup;
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

/// 只允许应用自身页面做顶层导航。主窗口是无边框置顶的，一旦被远程页面接管，
/// Esc 与本地 JS 都会失效，用户只能从托盘退出；外部链接一律交给系统浏览器。
pub(crate) fn navigation_allowed(url: &tauri::Url, dev_url: Option<&tauri::Url>) -> bool {
    let host = url.host_str().unwrap_or_default();
    match url.scheme() {
        // macOS / Linux 的生产页面
        "tauri" => host.eq_ignore_ascii_case("localhost"),
        // Windows 生产页面是 http(s)://tauri.localhost；开发时放行 devUrl 本身
        "http" | "https" => {
            if host.eq_ignore_ascii_case("tauri.localhost") {
                return true;
            }
            // 只有 debug 构建才可能跑 Vite 开发服务器，release 永不放行。
            // 端口不写死：devUrl 改过（比如避开被占用的 1420）守卫要跟着走。
            match dev_url {
                Some(dev) if cfg!(debug_assertions) => {
                    url.scheme() == dev.scheme()
                        && host.eq_ignore_ascii_case(dev.host_str().unwrap_or_default())
                        && url.port_or_known_default() == dev.port_or_known_default()
                }
                _ => false,
            }
        }
        _ => false,
    }
}

fn navigation_guard<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("navigation-guard")
        .on_navigation(|webview, url| {
            let allowed = navigation_allowed(url, webview.config().build.dev_url.as_ref());
            if !allowed {
                log::warn!("已阻止主窗口导航到 {url}");
            }
            allowed
        })
        .build()
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
    /// 启动时设置文件读不出来：本次运行不写回，避免默认值覆盖原文件。
    pub settings_read_failed: bool,
    /// 最近一次自动 Git 提交的结果，供设置页显示失败原因。
    pub(crate) git: Mutex<git_backup::GitCommitState>,
    /// 应用锁：配置状态、解锁截止时间与失败退避。
    pub(crate) app_lock: Mutex<app_lock::runtime::LockRuntime>,
    /// 串行化 Argon2 校验，避免并发尝试把 CPU 打满。
    pub(crate) lock_verify: tokio::sync::Mutex<()>,
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
        .plugin(navigation_guard())
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
            let settings_read_failed = loaded.read_failed;
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
                        if !settings_read_failed {
                            if let Err(save_err) = settings::save(&settings_path, &settings) {
                                log::error!("保存备用快捷键失败：{save_err}");
                            }
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
            let lock_runtime = app_lock::runtime::LockRuntime::from_store();
            if let Some(warning) = lock_runtime.warning() {
                log::warn!("{warning}");
            }

            handle.manage(AppState {
                settings: Mutex::new(settings),
                settings_path,
                last_source: Mutex::new(None),
                io: Arc::new(Mutex::new(())),
                sync: Arc::new(sync::runtime::SyncRuntime::new()),
                settings_write_seq: AtomicU64::new(0),
                settings_last_saved_seq: AtomicU64::new(0),
                settings_write_lock: Mutex::new(()),
                settings_read_failed,
                git: Mutex::new(git_backup::GitCommitState::default()),
                app_lock: Mutex::new(lock_runtime),
                lock_verify: tokio::sync::Mutex::new(()),
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
            commands::get_git_status,
            commands::init_notes_git,
            commands::update_git_backup,
            commands::update_hotkey,
            commands::update_capture_source,
            commands::update_markdown_mode,
            commands::list_notes,
            commands::search_notes,
            commands::delete_note_entry,
            commands::update_note_entry,
            commands::get_note_entries,
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
            commands::get_sync_file_states,
            commands::update_sync_config,
            commands::save_sync_password,
            commands::clear_sync_password,
            commands::test_sync_connection,
            commands::sync_now,
            commands::flush_sync_state,
            commands::app_version,
            commands::get_lock_status,
            commands::unlock,
            commands::lock_now,
            commands::set_app_lock_password,
            commands::disable_app_lock,
            commands::update_lock_validity,
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

#[cfg(test)]
mod tests {
    use super::navigation_allowed;

    fn allowed(url: &str) -> bool {
        let dev: tauri::Url = "http://localhost:1421/".parse().expect("devUrl 必须合法");
        navigation_allowed(&url.parse().expect("测试 URL 必须合法"), Some(&dev))
    }

    #[test]
    fn allows_only_the_app_origin() {
        // Windows 生产页面
        assert!(allowed("https://tauri.localhost/"));
        assert!(allowed("http://tauri.localhost/index.html"));
        // macOS / Linux 生产页面
        assert!(allowed("tauri://localhost/"));
        // 开发页面只认 devUrl 本身（端口来自配置，不是写死的 1420）
        assert_eq!(allowed("http://localhost:1421/"), cfg!(debug_assertions));
        assert!(!allowed("http://localhost:1420/"));
        assert!(!allowed("http://127.0.0.1:1421/"));
        // 远程与本地文件一律拒绝
        assert!(!allowed("https://example.com/"));
        assert!(!allowed("https://tauri.localhost.evil.example/"));
        assert!(!allowed("file:///C:/Windows/win.ini"));
        assert!(!allowed("javascript:alert(1)"));
    }

    #[test]
    fn without_a_dev_url_nothing_but_the_app_origin_is_allowed() {
        let url: tauri::Url = "http://localhost:1421/".parse().unwrap();
        assert!(!navigation_allowed(&url, None));
    }
}
