use chrono::Local;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};
use tauri::{AppHandle, Manager};

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    /// 保存时的 DPI 缩放。老配置没有该字段时为 None，读取时不换算。
    #[serde(default)]
    pub scale: Option<f64>,
}

fn default_true() -> bool {
    true
}

/// 背景不透明度的下限：再低正文在浅色桌面上就难以辨认。
pub const MIN_BACKGROUND_OPACITY: f64 = 0.6;

/// 设置文件里的主题只接受这三档，非法值回落跟随系统。
pub fn normalize_theme(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "light" => "light",
        "dark" => "dark",
        _ => "system",
    }
    .to_string()
}

/// 把不透明度夹到支持区间；非法浮点数按不透明处理。
pub fn clamp_opacity(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(MIN_BACKGROUND_OPACITY, 1.0)
    } else {
        1.0
    }
}

/// 云同步配置。密码不在这里：它只进 Windows 凭据管理器。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SyncSettings {
    pub enabled: bool,
    pub url: String,
    pub username: String,
    /// 自动同步间隔（分钟），只接受 5 / 15 / 60。
    pub interval_minutes: u32,
    /// 允许对非回环地址使用明文 `http://`。
    pub allow_insecure_http: bool,
    /// 冲突副本文件名里的设备标识；留空时用计算机名。
    pub device_name: String,
}

impl Default for SyncSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            username: String::new(),
            interval_minutes: 5,
            allow_insecure_http: false,
            device_name: String::new(),
        }
    }
}

impl SyncSettings {
    pub fn is_configured(&self) -> bool {
        !self.url.trim().is_empty() && !self.username.trim().is_empty()
    }

    /// 只接受 5 / 15 / 60，其余一律按 15 处理。
    pub fn normalize_interval(value: u32) -> u32 {
        match value {
            5 => 5,
            60 => 60,
            _ => 15,
        }
    }

    /// 生成冲突副本时使用的设备名：设置优先，其次计算机名。
    pub fn device_label(&self) -> String {
        let configured = self.device_name.trim();
        if !configured.is_empty() {
            return configured.to_string();
        }
        std::env::var("COMPUTERNAME")
            .ok()
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "device".to_string())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub notes_dir: String,
    pub hotkey: String,
    pub pinned: bool,
    pub capture_source: bool,
    pub markdown_mode: bool,
    pub history_preview: bool,
    /// 启动后静默检查更新；旧配置缺这个字段时默认开启。
    #[serde(default = "default_true")]
    pub auto_update: bool,
    /// 旧版本没有这个字段，缺省视为已完成引导；只有全新安装才会看到引导。
    #[serde(default = "default_true")]
    pub onboarded: bool,
    /// system / light / dark；旧配置缺这个字段时跟随系统。
    pub theme: String,
    /// 窗口失去焦点时自动收起；常驻模式下面不生效。
    pub auto_hide_on_blur: bool,
    /// 小窗与面板的背景不透明度，0.6–1.0。
    pub background_opacity: f64,
    pub window: Option<WindowGeometry>,
    /// WebDAV 同步参数；默认关闭，不配置时不产生任何网络请求。
    pub sync: SyncSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            notes_dir: String::new(),
            hotkey: "ctrl+alt+n".to_string(),
            pinned: false,
            capture_source: true,
            markdown_mode: false,
            history_preview: false,
            auto_update: true,
            onboarded: false,
            theme: "system".to_string(),
            auto_hide_on_blur: false,
            background_opacity: 1.0,
            window: None,
            sync: SyncSettings::default(),
        }
    }
}

/// 启动时读取设置的结果，附带需要提示用户的警告。
pub struct LoadedSettings {
    pub settings: Settings,
    pub warning: Option<String>,
}

pub fn settings_path(app: &AppHandle) -> PathBuf {
    app.path()
        .app_config_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("settings.json")
}

pub fn default_notes_dir(app: &AppHandle) -> PathBuf {
    app.path()
        .document_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("FlashNote")
}

pub fn load(app: &AppHandle) -> LoadedSettings {
    let path = settings_path(app);
    let mut loaded = load_from_path(&path);
    if loaded.settings.notes_dir.trim().is_empty() {
        loaded.settings.notes_dir = default_notes_dir(app).to_string_lossy().to_string();
    }
    loaded
}

/// 读取指定路径的设置文件。损坏时先备份原文件，再返回默认值。
pub(crate) fn load_from_path(path: &Path) -> LoadedSettings {
    let mut warning = None;

    let mut settings = match fs::read_to_string(path) {
        Ok(raw) => match serde_json::from_str::<Settings>(&raw) {
            // 手工编辑出的非法值只纠正字段，不把整份设置当成损坏
            Ok(mut parsed) => {
                parsed.theme = normalize_theme(&parsed.theme);
                parsed.background_opacity = clamp_opacity(parsed.background_opacity);
                parsed.sync.interval_minutes =
                    SyncSettings::normalize_interval(parsed.sync.interval_minutes);
                parsed
            }
            Err(err) => {
                log::error!("设置文件解析失败（{}）：{err}", path.display());
                warning = Some(match backup_corrupt(path) {
                    Some(backup) => format!(
                        "设置文件损坏，已备份为 {} 并恢复默认",
                        backup
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("settings.json.corrupt")
                    ),
                    None => "设置文件损坏，已恢复默认设置".to_string(),
                });
                recovered_default()
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Settings::default(),
        Err(err) => {
            log::warn!("读取设置文件失败（{}）：{err}", path.display());
            recovered_default()
        }
    };

    // 手改出的相对路径会跟着进程 CWD 漂移（自启动 vs 手动启动），读到时就固定成绝对路径
    settings.notes_dir = absolute_notes_dir(&settings.notes_dir);

    LoadedSettings { settings, warning }
}

/// 把相对路径固定成基于当前 CWD 的绝对路径。不碰文件系统，也不会引入 `\\?\` 前缀。
pub(crate) fn absolute_dir(raw: &str) -> PathBuf {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        return path;
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(&path))
        .unwrap_or(path)
}

fn absolute_notes_dir(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    absolute_dir(trimmed).to_string_lossy().to_string()
}

/// 配置文件存在但不可用时使用：不再重复触发首次运行引导。
fn recovered_default() -> Settings {
    Settings {
        onboarded: true,
        ..Settings::default()
    }
}

fn backup_corrupt(path: &Path) -> Option<PathBuf> {
    let stamp = Local::now().format("%Y%m%d-%H%M%S");
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("settings.json");
    let backup = path.with_file_name(format!("{file_name}.corrupt-{stamp}"));

    match fs::rename(path, &backup) {
        Ok(()) => {
            log::warn!("已备份损坏的设置文件到 {}", backup.display());
            Some(backup)
        }
        Err(err) => {
            log::error!("备份损坏的设置文件失败：{err}");
            None
        }
    }
}

pub fn save(path: &Path, settings: &Settings) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_vec_pretty(settings)
        .map_err(|err| std::io::Error::other(err.to_string()))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("flashnote-settings-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_file_returns_defaults_without_warning() {
        let dir = temp_dir("missing");
        let loaded = load_from_path(&dir.join("settings.json"));

        assert!(loaded.warning.is_none());
        assert_eq!(loaded.settings.hotkey, "ctrl+alt+n");
        assert!(loaded.settings.capture_source);
        // 文件不存在 = 全新安装，需要展示引导
        assert!(!loaded.settings.onboarded);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn relative_notes_dir_is_made_absolute() {
        let dir = temp_dir("relative");
        let path = dir.join("settings.json");
        fs::write(
            &path,
            r#"{"notesDir":"notes","hotkey":"ctrl+shift+k","pinned":true,"captureSource":false,"markdownMode":true,"window":null}"#,
        )
        .unwrap();

        let loaded = load_from_path(&path);

        assert!(loaded.warning.is_none());
        assert!(Path::new(&loaded.settings.notes_dir).is_absolute());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn valid_file_is_loaded_as_is() {
        let dir = temp_dir("valid");
        let path = dir.join("settings.json");
        fs::write(
            &path,
            r#"{"notesDir":"D:\\Notes","hotkey":"ctrl+shift+k","pinned":true,"captureSource":false,"markdownMode":true,"window":null}"#,
        )
        .unwrap();

        let loaded = load_from_path(&path);

        assert!(loaded.warning.is_none());
        assert_eq!(loaded.settings.notes_dir, "D:\\Notes");
        assert_eq!(loaded.settings.hotkey, "ctrl+shift+k");
        assert!(loaded.settings.pinned);
        assert!(!loaded.settings.capture_source);
        assert!(loaded.settings.markdown_mode);
        // 旧配置文件没有 autoUpdate 字段，默认开启检查
        assert!(loaded.settings.auto_update);
        // 旧配置文件没有 onboarded 字段，不应再次弹出引导
        assert!(loaded.settings.onboarded);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_is_backed_up_and_defaults_are_used() {
        let dir = temp_dir("corrupt");
        let path = dir.join("settings.json");
        fs::write(&path, r#"{"hotkey": "ctrl+alt+n", "#).unwrap();

        let loaded = load_from_path(&path);

        assert!(loaded.warning.is_some());
        assert_eq!(loaded.settings.hotkey, "ctrl+alt+n");
        assert!(loaded.settings.onboarded, "损坏恢复不应触发首次引导");
        assert!(!path.exists(), "损坏的原文件应被移走");

        let backups: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("settings.json.corrupt-")
            })
            .collect();
        assert_eq!(backups.len(), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_file_is_treated_as_corrupt() {
        let dir = temp_dir("empty");
        let path = dir.join("settings.json");
        fs::write(&path, "").unwrap();

        let loaded = load_from_path(&path);

        assert!(loaded.warning.is_some());
        assert!(!path.exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_is_atomic_and_round_trips() {
        let dir = temp_dir("save");
        let path = dir.join("settings.json");
        let settings = Settings {
            notes_dir: "D:\\Notes".to_string(),
            hotkey: "ctrl+shift+k".to_string(),
            pinned: true,
            capture_source: false,
            markdown_mode: true,
            history_preview: true,
            auto_update: false,
            onboarded: true,
            theme: "dark".to_string(),
            auto_hide_on_blur: true,
            background_opacity: 0.8,
            window: Some(WindowGeometry {
                x: -120,
                y: 40,
                w: 480,
                h: 360,
                scale: Some(1.0),
            }),
            sync: SyncSettings::default(),
        };

        save(&path, &settings).unwrap();

        assert!(path.exists());
        assert!(
            !path.with_extension("json.tmp").exists(),
            "重命名后不应残留 .json.tmp"
        );

        let loaded = load_from_path(&path);
        assert!(loaded.warning.is_none());
        assert_eq!(loaded.settings.hotkey, "ctrl+shift+k");
        assert!(loaded.settings.pinned);
        assert!(loaded.settings.history_preview);
        assert!(!loaded.settings.auto_update, "关闭状态应被保存");
        assert_eq!(loaded.settings.theme, "dark");
        assert!(loaded.settings.auto_hide_on_blur);
        assert_eq!(loaded.settings.background_opacity, 0.8);

        let geometry = loaded.settings.window.expect("几何应当被保存");
        assert_eq!(
            (geometry.x, geometry.y, geometry.w, geometry.h),
            (-120, 40, 480, 360)
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn partial_file_falls_back_to_defaults() {
        let dir = temp_dir("partial");
        let path = dir.join("settings.json");
        fs::write(&path, r#"{"hotkey":"ctrl+alt+m"}"#).unwrap();

        let loaded = load_from_path(&path);

        assert!(loaded.warning.is_none(), "缺字段不算损坏");
        assert_eq!(loaded.settings.hotkey, "ctrl+alt+m");
        assert_eq!(loaded.settings.notes_dir, "");
        assert!(loaded.settings.capture_source);
        assert!(!loaded.settings.markdown_mode);
        assert!(!loaded.settings.history_preview);
        assert!(loaded.settings.auto_update, "老配置缺字段时默认检查更新");
        assert!(loaded.settings.onboarded, "老配置缺字段时视为已完成引导");
        assert_eq!(loaded.settings.theme, "system", "老配置缺字段时跟随系统");
        assert!(!loaded.settings.auto_hide_on_blur);
        assert_eq!(loaded.settings.background_opacity, 1.0);
        assert!(loaded.settings.window.is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_theme_falls_back_to_system() {
        let dir = temp_dir("theme");
        let path = dir.join("settings.json");
        fs::write(&path, r#"{"theme":"neon"}"#).unwrap();

        let loaded = load_from_path(&path);

        assert!(loaded.warning.is_none(), "非法主题不算文件损坏");
        assert_eq!(loaded.settings.theme, "system");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn opacity_is_clamped_to_supported_range() {
        let dir = temp_dir("opacity");
        let path = dir.join("settings.json");
        fs::write(&path, r#"{"backgroundOpacity":0.2}"#).unwrap();
        assert_eq!(load_from_path(&path).settings.background_opacity, 0.6);

        fs::write(&path, r#"{"backgroundOpacity":1.4}"#).unwrap();
        assert_eq!(load_from_path(&path).settings.background_opacity, 1.0);

        assert_eq!(clamp_opacity(f64::NAN), 1.0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn theme_matching_is_case_insensitive() {
        assert_eq!(normalize_theme(" Light "), "light");
        assert_eq!(normalize_theme("DARK"), "dark");
        assert_eq!(normalize_theme(""), "system");
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let dir = temp_dir("unknown");
        let path = dir.join("settings.json");
        fs::write(
            &path,
            r#"{"hotkey":"ctrl+alt+n","futureOption":{"nested":true}}"#,
        )
        .unwrap();

        let loaded = load_from_path(&path);

        assert!(loaded.warning.is_none());
        assert_eq!(loaded.settings.hotkey, "ctrl+alt+n");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_settings_default_off_and_only_accept_known_intervals() {
        let dir = temp_dir("sync");
        let path = dir.join("settings.json");
        fs::write(&path, r#"{"sync":{"intervalMinutes":7}}"#).unwrap();

        let loaded = load_from_path(&path);
        assert!(!loaded.settings.sync.enabled, "同步默认关闭");
        assert!(!loaded.settings.sync.is_configured());
        assert_eq!(
            loaded.settings.sync.interval_minutes, 15,
            "未知间隔应回落到 15 分钟"
        );

        assert_eq!(SyncSettings::normalize_interval(5), 5);
        assert_eq!(SyncSettings::normalize_interval(60), 60);
        assert_eq!(SyncSettings::normalize_interval(0), 15);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_device_label_prefers_configured_name() {
        let configured = SyncSettings {
            device_name: "  Work-PC ".to_string(),
            ..SyncSettings::default()
        };
        assert_eq!(configured.device_label(), "Work-PC");

        let fallback = SyncSettings::default().device_label();
        assert!(!fallback.is_empty());
    }
}
