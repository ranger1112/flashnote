//! 保存后自动提交：给笔记目录留下可回溯的 Git 历史。
//!
//! 只做一件事——把 Markdown 变更提交到笔记目录自己的仓库。不 clone、不 push、
//! 不碰远端凭据：远端由用户自己配置，推送也由用户决定，保存路径里不该藏网络行为。
//!
//! 提交失败只记日志，绝不影响保存本身：笔记落盘永远优先于版本历史。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
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
/// 私有索引的临时文件名后缀，避免同一进程内多次调用互相覆盖。
static INDEX_SEQ: AtomicU64 = AtomicU64::new(0);

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

    // 仓库正处于合并冲突时不能提交：`git add` 会把带冲突标记的文件标成已解决，
    // 一次保存就等于替用户「解决」了冲突并把标记写进历史。
    let unmerged = run(Some(dir), &["ls-files", "-u"])?;
    if unmerged.status.success() && !unmerged.stdout.is_empty() {
        return Err("笔记仓库有未解决的合并冲突，已跳过自动提交".to_string());
    }

    // 用户索引干净时，提交后把私有索引同步回去，避免 `git status` 显示一批
    // 「已暂存的删除」；用户有暂存内容时绝不碰真实索引。
    let index_clean = run(Some(dir), &["diff", "--no-ext-diff", "--cached", "--quiet"])
        .map(|out| out.status.code() == Some(0))
        .unwrap_or(false);

    let head = match run(Some(dir), &["rev-parse", "--verify", "HEAD"]) {
        Ok(out) if out.status.success() => {
            Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
        }
        _ => None,
    };

    let index = new_private_index();
    let result = commit_with_index(dir, head.as_deref(), &index, index_clean);
    let _ = fs::remove_file(&index);
    let _ = fs::remove_file(format!("{}.lock", index.display()));
    result
}

/// 用私有索引提交，真实索引保持原样；这样用户在笔记仓库里的 `git add -p`
/// 不会被一次保存覆盖。hooks 与外部 filter 的风险也在这里一并处理。
fn commit_with_index(
    dir: &Path,
    head: Option<&str>,
    index: &Path,
    index_clean: bool,
) -> Result<bool, String> {
    let mut envs: Vec<(&str, String)> = vec![
        ("GIT_INDEX_FILE", index.to_string_lossy().into_owned()),
        // 仓库配置里的 core.worktree 可以把操作引到别的目录；强制用笔记目录。
        ("GIT_WORK_TREE", dir.to_string_lossy().into_owned()),
    ];

    let read = if head.is_some() {
        run_with_env(Some(dir), &["read-tree", "HEAD"], &envs)?
    } else {
        run_with_env(Some(dir), &["read-tree", "--empty"], &envs)?
    };
    if !read.status.success() {
        return Err(format!("git read-tree 失败：{}", stderr(&read)));
    }

    // 仓库里的 .gitattributes / 配置可以给 Markdown 指定外部 clean filter，
    // `git add` 会执行它。发现任何 filter 属性就跳过本轮，绝不在用户仓库里执行命令。
    let names = markdown_names(dir)?;
    if has_external_filter(dir, &names)? {
        return Err("笔记仓库为 Markdown 配置了 Git filter，已跳过自动提交".to_string());
    }

    let added = run_with_env(Some(dir), &["add", "-A", "--", "*.md"], &envs)?;
    if !added.status.success() {
        // 目录里一个 .md 都没有时 git 会报 pathspec 不匹配；这不是错误
        let message = stderr(&added);
        if message.contains("did not match any files") {
            return Ok(false);
        }
        return Err(format!("git add 失败：{message}"));
    }

    // 暂存区与 HEAD 一致：这次保存没有产生新内容（例如重复确认了同一条删除）
    let staged = run_with_env(
        Some(dir),
        &["diff", "--no-ext-diff", "--cached", "--quiet", "--", "*.md"],
        &envs,
    )?;
    if staged.status.success() {
        return Ok(false);
    }

    let changed = run_with_env(
        Some(dir),
        &[
            "diff",
            "--no-ext-diff",
            "--cached",
            "--name-only",
            "--",
            "*.md",
        ],
        &envs,
    )?;
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
    if !has_identity(dir) {
        envs.extend([
            ("GIT_AUTHOR_NAME", "FlashNote".to_string()),
            ("GIT_AUTHOR_EMAIL", "flashnote@localhost".to_string()),
            ("GIT_COMMITTER_NAME", "FlashNote".to_string()),
            ("GIT_COMMITTER_EMAIL", "flashnote@localhost".to_string()),
        ]);
    }

    let out = run_with_env(
        Some(dir),
        &[
            "commit",
            "-q",
            "--no-verify",
            "-m",
            message.as_str(),
            "--",
            "*.md",
        ],
        &envs,
    )?;
    if !out.status.success() {
        return Err(format!("git commit 失败：{}", stderr(&out)));
    }

    if index_clean {
        sync_private_index(dir, index);
    }
    Ok(true)
}

/// 把提交后的私有索引写回真实索引。只在用户索引原本干净时调用，
/// 失败只影响 `git status` 的显示，不影响已经产生的提交。
fn sync_private_index(dir: &Path, private: &Path) {
    let Ok(out) = run(Some(dir), &["rev-parse", "--git-path", "index"]) else {
        return;
    };
    if !out.status.success() {
        return;
    }
    let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if raw.is_empty() {
        return;
    }
    let real = {
        let path = PathBuf::from(&raw);
        if path.is_absolute() {
            path
        } else {
            dir.join(path)
        }
    };
    let tmp = real.with_file_name(format!(".flashnote-index-{}", std::process::id()));
    if fs::copy(private, &tmp).is_err() {
        let _ = fs::remove_file(&tmp);
        return;
    }
    if fs::rename(&tmp, &real).is_err() {
        let _ = fs::remove_file(&tmp);
        log::warn!("同步自动提交后的索引失败，git status 可能仍显示旧状态");
    }
}

fn new_private_index() -> PathBuf {
    let seq = INDEX_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "flashnote-git-index-{}-{nanos}-{seq}",
        std::process::id()
    ))
}

fn disabled_hooks_path() -> PathBuf {
    std::env::temp_dir().join("flashnote-disabled-git-hooks")
}

/// 列出工作区里的 Markdown 路径（相对仓库根），用于检查 filter 属性。
/// 跳过符号链接/重解析点，避免顺着链接读到笔记目录外。
fn markdown_names(dir: &Path) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        let entries =
            fs::read_dir(&current).map_err(|error| format!("无法读取笔记目录：{error}"))?;
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_dir() {
                // 不进入仓库内部目录：没有笔记，还会拖慢每次保存。
                if path.file_name().and_then(|name| name.to_str()) != Some(".git")
                    && !crate::files::is_reparse_point(&path)
                {
                    pending.push(path);
                }
                continue;
            }
            if !meta.file_type().is_file() {
                continue;
            }
            if path.extension().and_then(|value| value.to_str()) != Some("md") {
                continue;
            }
            let relative = path
                .strip_prefix(dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            names.push(relative);
        }
    }
    names.sort();
    Ok(names)
}

fn has_external_filter(dir: &Path, names: &[String]) -> Result<bool, String> {
    if names.is_empty() {
        return Ok(false);
    }
    let mut args: Vec<&str> = vec!["check-attr", "-z", "filter", "--"];
    args.extend(names.iter().map(String::as_str));
    let out = run(Some(dir), &args)?;
    if !out.status.success() {
        return Err(format!("检查 Git filter 失败：{}", stderr(&out)));
    }
    let parts: Vec<&[u8]> = out.stdout.split(|byte| *byte == 0).collect();
    for chunk in parts.chunks(3) {
        if chunk.len() == 3 && String::from_utf8_lossy(chunk[2]) != "unspecified" {
            return Ok(true);
        }
    }
    Ok(false)
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
    // 有些安装器会把 user.name 设成空字符串：exit 0 且输出只有一个换行，
    // 只判 is_empty 会误以为身份可用，之后每次提交都报 empty ident name。
    let read = |key: &str| {
        run(Some(dir), &["config", key])
            .map(|out| {
                out.status.success() && !String::from_utf8_lossy(&out.stdout).trim().is_empty()
            })
            .unwrap_or(false)
    };
    read("user.name") && read("user.email")
}

fn run(dir: Option<&Path>, args: &[&str]) -> Result<Output, String> {
    run_with_env(dir, args, &[])
}

fn run_with_env(
    dir: Option<&Path>,
    args: &[&str],
    envs: &[(&str, String)],
) -> Result<Output, String> {
    let mut command = Command::new("git");
    if dir.is_some() {
        // 笔记目录可能是共享盘/NAS：仓库里的 hooks、fsmonitor 和签名配置
        // 都以用户权限执行，自动提交一律不信任它们。
        let hooks = format!("core.hooksPath={}", disabled_hooks_path().to_string_lossy());
        command.args([
            "-c",
            hooks.as_str(),
            "-c",
            "core.fsmonitor=false",
            "-c",
            "commit.gpgsign=false",
        ]);
    }
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in envs {
        command.env(key, value);
    }
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
    fn commit_does_not_sweep_user_staged_files() {
        if !available() {
            return;
        }
        let dir = temp_dir("stage");
        init(&dir).unwrap();
        fs::write(dir.join("2026-10-31.md"), "note").unwrap();
        fs::write(dir.join("secret.txt"), "do not commit me").unwrap();

        // 用户自己把非 Markdown 文件放进暂存区
        run(Some(&dir), &["add", "--", "secret.txt"]).unwrap();

        assert!(commit(&dir).unwrap());
        let tree = run(Some(&dir), &["ls-tree", "-r", "--name-only", "HEAD"]).unwrap();
        let files = String::from_utf8_lossy(&tree.stdout);
        assert!(files.contains("2026-10-31.md"));
        assert!(
            !files.contains("secret.txt"),
            "不该把用户暂存的文件卷进笔记提交"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_leaves_a_dirty_user_index_alone() {
        if !available() {
            return;
        }
        let dir = temp_dir("private-index");
        init(&dir).unwrap();
        fs::write(dir.join("2026-11-01.md"), "note\n").unwrap();
        // 模拟用户自己把文件放进暂存区
        run(Some(&dir), &["add", "--", "2026-11-01.md"]).unwrap();
        let before = run(Some(&dir), &["ls-files", "-s"]).unwrap().stdout;

        assert!(commit(&dir).unwrap());

        let after = run(Some(&dir), &["ls-files", "-s"]).unwrap().stdout;
        assert_eq!(before, after, "自动提交不应改写用户索引");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_keeps_status_clean_when_index_was_clean() {
        if !available() {
            return;
        }
        let dir = temp_dir("clean-index");
        init(&dir).unwrap();
        fs::write(dir.join("2026-11-01.md"), "note\n").unwrap();

        assert!(commit(&dir).unwrap());

        let status = run(Some(&dir), &["status", "--porcelain"]).unwrap();
        assert!(
            status.stdout.is_empty(),
            "提交后工作区应保持干净：{}",
            String::from_utf8_lossy(&status.stdout)
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_refuses_external_markdown_filter() {
        if !available() {
            return;
        }
        let dir = temp_dir("filter");
        init(&dir).unwrap();
        fs::write(dir.join("2026-11-01.md"), "note\n").unwrap();
        fs::write(dir.join(".gitattributes"), "*.md filter=evil\n").unwrap();

        let error = commit(&dir).unwrap_err();
        assert!(error.contains("filter"), "{error}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_outside_a_repo_is_an_error() {
        let dir = temp_dir("norepo");
        assert!(commit(&dir).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
