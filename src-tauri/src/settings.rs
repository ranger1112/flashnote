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
}

fn default_true() -> bool {
    true
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
    /// 旧版本没有这个字段，缺省视为已完成引导；只有全新安装才会看到引导。
    #[serde(default = "default_true")]
    pub onboarded: bool,
    pub window: Option<WindowGeometry>,
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
            onboarded: false,
            window: None,
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

    let settings = match fs::read_to_string(path) {
        Ok(raw) => match serde_json::from_str::<Settings>(&raw) {
            Ok(parsed) => parsed,
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

    LoadedSettings { settings, warning }
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
            onboarded: true,
            window: Some(WindowGeometry {
                x: -120,
                y: 40,
                w: 480,
                h: 360,
            }),
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
        assert!(loaded.settings.onboarded, "老配置缺字段时视为已完成引导");
        assert!(loaded.settings.window.is_none());

        let _ = fs::remove_dir_all(&dir);
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
}
