//! 文件与系统调用：打开目录/文件、URL 白名单、笔记改写（备份 + 原子替换）。
//!
//! 所有笔记文件读写都经过 `io_guard` 串行化；阻塞调用由上层放进
//! `spawn_blocking` 执行，避免占用 UI 线程或 tokio worker。

use crate::{notes, settings, AppState};
use std::{
    path::{Path, PathBuf},
    sync::MutexGuard,
};
use tauri::{AppHandle, Manager};

/// 笔记文件的读写都串行化：命令改成 async 之后，追加与改写是真的可能并发的。
pub(crate) fn io_guard(state: &AppState) -> MutexGuard<'_, ()> {
    state
        .io
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn open_with_explorer(path: &Path) -> Result<(), String> {
    // explorer 不认 `\\?\` verbatim 前缀，遇到会直接报「找不到文件」；先剥掉再传
    let display = path.to_string_lossy();
    let target = display.strip_prefix(r"\\?\").unwrap_or(&display);
    std::process::Command::new("explorer")
        .arg(target)
        .spawn()
        .map(|_| ())
        .map_err(|err| err.to_string())
}

/// 只允许把 http(s) 与 mailto 交给系统打开，其余协议（file:、javascript: 等）一律拒绝。
pub(crate) fn external_url_allowed(url: &str) -> bool {
    const ALLOWED: [&str; 3] = ["http://", "https://", "mailto:"];
    !url.is_empty()
        && url.len() <= 2048
        && !url.chars().any(|ch| ch.is_control() || ch == '"')
        && ALLOWED.iter().any(|prefix| {
            url.get(..prefix.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
        })
}

#[cfg(windows)]
pub(crate) fn open_with_shell(target: &str) -> Result<(), String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    fn wide(value: &str) -> Vec<u16> {
        OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    // 直接调 ShellExecuteW，不经过 cmd，避免 URL 里的 & | ^ 被当成命令分隔符
    let operation = wide("open");
    let file = wide(target);
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // Win32 约定：返回值 <= 32 都算失败
    if (result as isize) <= 32 {
        Err("系统未能打开该目标".to_string())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
pub(crate) fn open_with_shell(_target: &str) -> Result<(), String> {
    Err("当前平台暂不支持调用系统默认程序".to_string())
}

/// 交给系统默认关联程序打开（Markdown 文件通常是编辑器）。
fn open_with_default_app(path: &Path) -> Result<(), String> {
    // 同样走 ShellExecuteW：`cmd /C start` 会二次解析路径里的 & % ^
    open_with_shell(&path.to_string_lossy())
}

/// 确保当天的日期文件存在：只在创建成功时写入标题，已有文件原样保留。
fn ensure_note_file(dir: &Path, date: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|err| err.to_string())?;
    let path = dir.join(format!("{date}.md"));
    // create_new 而不是 exists + write：文件已存在时保持原样，不会截断当天笔记
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            use std::io::Write;
            file.write_all(format!("# {date}\n").as_bytes())
                .map_err(|err| format!("创建今日笔记失败：{err}"))?;
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(format!("创建今日笔记失败：{err}")),
    }
    Ok(path)
}

pub fn open_today_note_impl(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let notes_dir = state
        .settings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .notes_dir
        .clone();
    let (date, _) = notes::now_parts();
    let dir = PathBuf::from(notes_dir);

    let path = {
        // 与追加/改写共用一把锁：否则创建检查与并发 rewrite 会互相覆盖
        let _guard = io_guard(&state);
        ensure_note_file(&dir, &date)?
    };

    open_with_default_app(&path)
}

pub fn open_notes_dir_impl(app: &AppHandle) -> Result<(), String> {
    let notes_dir = app
        .state::<AppState>()
        .settings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .notes_dir
        .clone();
    let dir = PathBuf::from(notes_dir);
    std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    open_with_explorer(&dir)
}

pub(crate) fn open_note_file_impl(state: &AppState, path: &str) -> Result<(), String> {
    let notes_dir = PathBuf::from(
        &state
            .settings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .notes_dir,
    );
    let target = resolve_within(&notes_dir, path)?;
    open_with_explorer(&target)
}

pub fn open_log_dir_impl(app: &AppHandle) -> Result<(), String> {
    let dir = app.path().app_log_dir().map_err(|err| err.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    open_with_explorer(&dir)
}

/// 校验目录存在且可写，返回创建好的路径。
pub(crate) fn ensure_writable_dir(path: &str) -> Result<PathBuf, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("目录不能为空".to_string());
    }

    // 相对路径会跟着进程 CWD 走（自启动和手动启动不同），这里统一转成绝对路径。
    // 用绝对化而不是 canonicalize，避免拿到 explorer 不认的 `\\?\` 前缀。
    let dir = settings::absolute_dir(trimmed);
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

/// 把目标文件解析到笔记目录内。`..` 穿越、指向目录外的绝对路径都会被拒绝。
pub(crate) fn resolve_within(dir: &Path, path: &str) -> Result<PathBuf, String> {
    let dir = dir
        .canonicalize()
        .map_err(|err| format!("笔记目录不可用：{err}"))?;
    let target = PathBuf::from(path)
        .canonicalize()
        .map_err(|err| format!("文件不存在：{err}"))?;
    if !target.starts_with(&dir) {
        return Err("文件不在笔记目录中".to_string());
    }
    Ok(target)
}

/// 在笔记目录内改写单个文件：先备份，再原子替换。
pub(crate) fn rewrite_note<F>(state: &AppState, path: &str, update: F) -> Result<(), String>
where
    F: FnOnce(&str) -> Option<String>,
{
    let notes_dir = PathBuf::from(
        &state
            .settings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .notes_dir,
    );
    let dir = notes_dir
        .canonicalize()
        .map_err(|err| format!("笔记目录不可用：{err}"))?;
    let target = resolve_within(&dir, path)?;
    // 读-改-写必须独占，否则两次并发删除会互相覆盖
    let _guard = io_guard(state);

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

    // 临时文件带唯一后缀：上次异常退出残留的同名 tmp 不会挡住这次写入
    let unique = chrono::Local::now().format("%Y%m%d-%H%M%S%.6f");
    let tmp = target.with_file_name(format!("{stem}.md.tmp-{unique}"));
    if let Err(err) = std::fs::write(&tmp, updated.as_bytes()) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("写入失败：{err}"));
    }
    if let Err(err) = std::fs::rename(&tmp, &target) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("写入失败：{err}"));
    }
    // 解析缓存按 mtime+size 判断，但同一秒内的改写可能撞上旧值，这里直接失效
    notes::invalidate_cache(&target);

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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("flashnote-files-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn allows_http_https_and_mailto() {
        assert!(external_url_allowed("https://example.com/a?b=1"));
        assert!(external_url_allowed("HTTP://example.com"));
        assert!(external_url_allowed("mailto:hi@example.com"));
    }

    #[test]
    fn rejects_other_schemes_and_control_chars() {
        assert!(!external_url_allowed(""));
        assert!(!external_url_allowed("#anchor"));
        assert!(!external_url_allowed("javascript:alert(1)"));
        assert!(!external_url_allowed(
            "file:///C:/Windows/System32/calc.exe"
        ));
        // 换行属于控制字符，不能在 URL 里悄悄夹带
        assert!(!external_url_allowed("https://example.com/\ncalc.exe"));
        assert!(!external_url_allowed("https://example.com/\"x"));
        assert!(!external_url_allowed(&format!(
            "https://example.com/{}",
            "a".repeat(2048)
        )));
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
    fn resolve_within_accepts_file_in_notes_dir() {
        let dir = temp_dir("resolve-inside");
        let file = dir.join("2026-10-21.md");
        std::fs::write(&file, "# 2026-10-21\n").unwrap();

        let target = resolve_within(&dir, file.to_str().unwrap()).unwrap();
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
    fn ensure_note_file_preserves_existing_notes() {
        let dir = temp_dir("today-existing");
        let existing = dir.join("2026-10-23.md");
        std::fs::write(&existing, "# 2026-10-23\n\n- 08:00 已经写过一条\n").unwrap();

        let path = ensure_note_file(&dir, "2026-10-23").unwrap();

        assert_eq!(path, existing);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("已经写过一条"), "已有内容不能被截断：{text}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_note_file_creates_header_once() {
        let dir = temp_dir("today-new");

        let path = ensure_note_file(&dir, "2026-10-23").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# 2026-10-23\n");

        let again = ensure_note_file(&dir, "2026-10-23").unwrap();
        assert_eq!(again, path);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# 2026-10-23\n",
            "第二次调用不应再写一个标题"
        );

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
