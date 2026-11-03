//! 保存后自动提交：给笔记目录留下可回溯的 Git 历史。
//!
//! 只做一件事——把 Markdown 变更提交到笔记目录自己的仓库。不 clone、不 push、
//! 不碰远端凭据：远端由用户自己配置，推送也由用户决定，保存路径里不该藏网络行为。
//!
//! 提交失败只记日志，绝不影响保存本身：笔记落盘永远优先于版本历史。

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::AppState;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// CREATE_NO_WINDOW：后台调 git 时不要弹出黑色控制台窗口。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 同一时刻只允许一个 git 进程：并发写 `.git/index.lock` 会直接失败。
static GIT_LOCK: Mutex<()> = Mutex::new(());

/// 设置面板里展示的 Git 状态。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitStatus {
    /// 系统里能找到 `git` 命令。
    pub available: bool,
    /// 笔记目录本身是一个仓库（存在 `.git`）。
    pub repo: bool,
}

pub(crate) fn status(dir: &Path) -> GitStatus {
    GitStatus {
        available: available(),
        repo: is_repo(dir),
    }
}

pub(crate) fn is_repo(dir: &Path) -> bool {
    dir.join(".git").exists()
}

/// 系统里有没有 git。找不到时设置项直接禁用，而不是等保存后才报错。
pub(crate) fn available() -> bool {
    run(None, &["--version"])
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// 初始化仓库；已经是仓库时直接返回成功。
pub(crate) fn init(dir: &Path) -> Result<(), String> {
    if !dir.exists() {
        return Err("笔记目录不存在".to_string());
    }
    if is_repo(dir) {
        return Ok(());
    }
    let out = run(Some(dir), &["init", "-q"])?;
    if !out.status.success() {
        return Err(format!("git init 失败：{}", stderr(&out)));
    }
    Ok(())
}

/// 提交当前改动；返回是否真的产生了提交。
///
/// 只提交 `*.md`：`.bak-*`、`.tmp-*` 这类备份和改写到一半的临时文件不该进历史。
/// 没有变化时不产生空提交。
pub(crate) fn commit(dir: &Path) -> Result<bool, String> {
    if !is_repo(dir) {
        return Err("笔记目录还不是 Git 仓库".to_string());
    }
    let _guard = GIT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let added = run(Some(dir), &["add", "-A", "--", "*.md"])?;
    if !added.status.success() {
        // 目录里一个 .md 都没有时 git 会报 pathspec 不匹配；这不是错误
        let message = stderr(&added);
        if message.contains("did not match any files") {
            return Ok(false);
        }
        return Err(format!("git add 失败：{message}"));
    }

    // 暂存区与 HEAD 一致：这次保存没有产生新内容（例如重复确认了同一条删除）
    let staged = run(Some(dir), &["diff", "--cached", "--quiet"])?;
    if staged.status.success() {
        return Ok(false);
    }

    let changed = run(Some(dir), &["diff", "--cached", "--name-only"])?;
    let count = String::from_utf8_lossy(&changed.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    let message = format!(
        "笔记 {}（{count} 个文件）",
        chrono::Local::now().format("%Y-%m-%d %H:%M")
    );

    // 没配过 user.name / user.email 的机器直接 commit 会失败；用一次性身份兜底，
    // 不改用户全局配置。
    let mut args: Vec<&str> = Vec::new();
    if !has_identity(dir) {
        args.extend([
            "-c",
            "user.name=FlashNote",
            "-c",
            "user.email=flashnote@localhost",
        ]);
    }
    args.extend(["commit", "-q", "-m", &message]);

    let out = run(Some(dir), &args)?;
    if !out.status.success() {
        return Err(format!("git commit 失败：{}", stderr(&out)));
    }
    Ok(true)
}

/// 保存成功后调度一次提交；未开启或目录不是仓库时静默跳过。
pub(crate) fn schedule(app: &AppHandle) {
    let Some(dir) = enabled_notes_dir(app) else {
        return;
    };
    tauri::async_runtime::spawn_blocking(move || {
        if let Err(error) = commit(&dir) {
            // 版本历史失败不该打扰正在记笔记的人，只留日志
            log::warn!("自动提交失败：{error}");
        }
    });
}

fn enabled_notes_dir(app: &AppHandle) -> Option<PathBuf> {
    let state = app.state::<AppState>();
    let settings = state
        .settings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    (settings.git_backup && !settings.notes_dir.trim().is_empty())
        .then(|| PathBuf::from(settings.notes_dir))
}

fn has_identity(dir: &Path) -> bool {
    let read = |key: &str| {
        run(Some(dir), &["config", key])
            .map(|out| out.status.success() && !out.stdout.is_empty())
            .unwrap_or(false)
    };
    read("user.name") && read("user.email")
}

fn run(dir: Option<&Path>, args: &[&str]) -> Result<Output, String> {
    let mut command = Command::new("git");
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command
        .output()
        .map_err(|error| format!("无法运行 git：{error}"))
}

/// git 的报错主要在 stderr；有些版本也会写 stdout。
fn stderr(out: &Output) -> String {
    let text = String::from_utf8_lossy(&out.stderr);
    let text = text.trim();
    if text.is_empty() {
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("flashnote-git-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn init_creates_a_repo_only_once() {
        if !available() {
            return; // 没装 git 的环境跳过
        }
        let dir = temp_dir("init");
        assert!(!is_repo(&dir));
        init(&dir).unwrap();
        assert!(is_repo(&dir));
        // 重复初始化是幂等的
        init(&dir).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn commits_markdown_and_skips_backups_and_empty_runs() {
        if !available() {
            return;
        }
        let dir = temp_dir("commit");
        init(&dir).unwrap();
        fs::write(dir.join("2026-10-31.md"), "- 10:00 记一笔 #工作\n").unwrap();
        fs::write(dir.join("2026-10-31.md.bak-20261031100000"), "backup").unwrap();

        assert!(commit(&dir).unwrap(), "首次提交应产生记录");
        assert!(!commit(&dir).unwrap(), "没有变化时不应产生空提交");

        let tree = run(Some(&dir), &["ls-tree", "-r", "--name-only", "HEAD"]).unwrap();
        let files = String::from_utf8_lossy(&tree.stdout);
        assert!(files.contains("2026-10-31.md"));
        assert!(!files.contains(".bak-"), "备份文件不该进历史");

        fs::write(dir.join("2026-10-31.md"), "- 11:00 又记一笔\n").unwrap();
        assert!(commit(&dir).unwrap(), "内容变化后应再次提交");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_outside_a_repo_is_an_error() {
        let dir = temp_dir("norepo");
        assert!(commit(&dir).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
