//! 云同步：本地状态、决策表、条目级合并与后端抽象。
//!
//! 引擎只做「文件级比较 + 条目级合并」，不关心传输细节；WebDAV 在 `webdav`
//! 模块里实现 `SyncBackend`，未来的闪记云也复用同一套引擎。
//!
//! 核心安全约定：
//! - 远端列表拿不到时整轮中止，不做任何删除；
//! - 本地文件读不了（非 UTF-8、超大、权限）时按「未知」处理，不传播删除；
//! - 单轮删除超过 5 个且超过已同步文件 30% 时中止；
//! - 服务器不支持条件写入时只上传/下载，绝不传播删除。

pub(crate) mod backend;
pub(crate) mod merge;
pub(crate) mod runtime;
pub(crate) mod secrets;
pub(crate) mod state;
pub(crate) mod webdav;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use chrono::Local;
use serde::Serialize;

use backend::{Capabilities, Expect, RemoteMeta, RemoteVersion, SyncBackend, SyncError};
use state::{mtime_ns, now_iso, sha256_base64, write_atomic, FileRecord, StateStore, SyncState};

use crate::notes;

/// 与协议一致：单个笔记文件明文上限 2 MiB。
pub(crate) const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// 删除安全阀：超过这个数量、且占比超过阈值才中止。
const DELETE_MIN_FILES: usize = 5;
const DELETE_RATIO_PERCENT: usize = 30;
/// 已跟踪文件同时消失时，即使总数不到阈值也中止：这是「目录被清空 / 挂载失败」
/// 最典型的形态，小笔记库否则完全没有保护。
const DELETE_ALL_MIN_FILES: usize = 3;

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SyncSummary {
    pub uploaded: usize,
    pub downloaded: usize,
    pub merged: usize,
    pub deleted: usize,
    pub skipped: usize,
    pub conflicts: usize,
    pub aborted: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

impl SyncSummary {
    fn new() -> Self {
        Self {
            started_at: now_iso(),
            ..Self::default()
        }
    }

    fn error(&mut self, name: &str, error: impl std::fmt::Display) {
        self.errors.push(format!("{name}：{error}"));
    }

    /// 预期内的「这一轮做不完」不算失败：预算耗尽与超大文件只记警告。
    fn record(&mut self, name: &str, error: SyncError) {
        match error {
            SyncError::Budget | SyncError::TooLarge => {
                self.warnings.push(format!("{name}：{error}"));
            }
            other => self.error(name, other),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Skip,
    Upload,
    Download,
    Merge,
    DeleteRemote,
    /// 本地文件被删、远端又被改过：把远端内容下载回来，不删除。
    RestoreLocal,
    /// 远端被删、本地未改：本地文件移入 trash 后删除。
    TrashLocal,
    /// 远端被删、本地改过：以本地为准重新上传。
    UploadRevive,
    /// 两侧都已删除，清掉状态记录。
    Forget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LocalState {
    Absent,
    Unchanged,
    Changed,
}

/// 纯决策表，便于逐行测试。
fn decide(
    caps: &Capabilities,
    has_record: bool,
    local: LocalState,
    remote_present: bool,
    remote_changed: bool,
) -> Action {
    if !has_record {
        return match (local != LocalState::Absent, remote_present) {
            // 列表不可靠时无法断定远端真的没有这个文件：先 exists() 复核再写，
            // 否则首轮同步或列表被截断时会覆盖另一台设备刚创建的笔记。
            (true, false) if caps.reliable_listing => Action::Upload,
            (true, false) => Action::UploadRevive,
            (true, true) => Action::Merge,
            (false, true) => Action::Download,
            (false, false) => Action::Skip,
        };
    }

    if !remote_present {
        // 列表不可靠时不能把「没列出来」当成删除；但本地有修改仍应尝试上传，
        // apply 会先向远端确认一次，确认存在就放弃覆盖。
        if !caps.reliable_listing {
            return match local {
                LocalState::Changed => Action::UploadRevive,
                _ => Action::Skip,
            };
        }
        return match local {
            LocalState::Absent => Action::Forget,
            LocalState::Changed => Action::UploadRevive,
            LocalState::Unchanged => {
                if caps.supports_deletion() {
                    Action::TrashLocal
                } else {
                    // 弱服务器只上传：把本地内容补回远端。列表可能漏项，上传前
                    // 先确认远端确实没有，避免覆盖另一台设备刚写的内容。
                    Action::UploadRevive
                }
            }
        };
    }

    match (local, remote_changed) {
        (LocalState::Absent, false) => {
            if caps.supports_deletion() {
                Action::DeleteRemote
            } else {
                // 无法传播删除；保留记录，避免下一轮把远端副本当成新文件下载回来。
                Action::Skip
            }
        }
        (LocalState::Absent, true) => Action::RestoreLocal,
        (LocalState::Unchanged, false) => Action::Skip,
        (LocalState::Unchanged, true) => Action::Download,
        (LocalState::Changed, false) => Action::Upload,
        (LocalState::Changed, true) => Action::Merge,
    }
}

pub(crate) struct LocalFile {
    pub bytes: Vec<u8>,
    pub text: String,
    pub sha256: String,
}

#[derive(Default)]
struct LocalScan {
    files: BTreeMap<String, LocalFile>,
    /// 读不了的文件：按未知处理，不参与删除判断。
    skipped: BTreeSet<String>,
    warnings: Vec<String>,
}

/// 改写/删除本地文件前对当前内容的期望。
///
/// 决策基于同步开始时的一次扫描；网络阶段可能持续数秒到数分钟，期间用户完全
/// 可能又保存了笔记。真正落盘前必须在同一把锁内复核，否则会用旧快照覆盖新内容。
enum LocalExpect {
    /// 文件必须仍不存在。
    Absent,
    /// 文件内容必须仍是扫描时的摘要。
    Sha(String),
    /// 不检查（新建文件）。
    Any,
}

/// 把本地文件 I/O 放进阻塞线程池；网络请求仍然保持 async。
async fn blocking<T, F>(task: F) -> Result<T, SyncError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(task)
        .await
        .map_err(|error| SyncError::Io(format!("后台任务失败：{error}")))
}

/// 笔记文件名的白名单校验。
///
/// 远端列表是不可信输入，而名字会直接参与 `notes_dir`、`base/`、`trash/` 的
/// 路径拼接：必须先拒绝分隔符、`..` 和协议之外的冲突后缀，否则一个
/// `%2e%2e%2f` 编码的 href 就能把文件写到笔记目录之外。
pub(crate) fn is_valid_note_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 128 {
        return false;
    }
    if name.starts_with('.') || name.contains("..") {
        return false;
    }
    if name
        .chars()
        .any(|ch| ch.is_control() || ch == '/' || ch == '\\')
    {
        return false;
    }

    let Some(stem) = name.strip_suffix(".md") else {
        return false;
    };
    let Some(date) = stem.get(..10) else {
        return false;
    };
    if chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_err() {
        return false;
    }
    if stem.len() == 10 {
        return true;
    }

    let Some(suffix) = stem
        .get(10..)
        .and_then(|rest| rest.strip_prefix(".conflict-"))
    else {
        return false;
    };
    !suffix.is_empty()
        && suffix.len() <= 48
        && suffix
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

/// 只同步日期笔记与协议允许的冲突副本；`.bak-*`、`.tmp-*`、其它 Markdown 不碰。
fn is_sync_name(name: &str) -> bool {
    is_valid_note_name(name)
}

fn scan_local(dir: &Path) -> Result<LocalScan, SyncError> {
    let entries = fs::read_dir(dir)
        .map_err(|error| SyncError::Aborted(format!("无法读取笔记目录：{error}")))?;
    let mut scan = LocalScan::default();

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                scan.warnings
                    .push(format!("读取目录项失败（{error}），已忽略"));
                continue;
            }
        };
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            scan.warnings
                .push("跳过文件名不是 UTF-8 的文件".to_string());
            continue;
        };
        if !is_sync_name(name) {
            continue;
        }
        let name = name.to_string();

        // 元数据读取失败时按「未知」处理：不参与删除判断，否则一次瞬时错误
        // 会被当成「本地文件已删」并传播到远端。
        let meta = match entry.metadata() {
            Ok(meta) => meta,
            Err(error) => {
                scan.warnings
                    .push(format!("{name}：无法读取文件信息（{error}）"));
                scan.skipped.insert(name);
                continue;
            }
        };
        if !meta.is_file() {
            // 目录或其它类型占用了笔记名：同样按未知处理，绝不当作已删除。
            scan.warnings.push(format!("{name}：不是普通文件，已跳过"));
            scan.skipped.insert(name);
            continue;
        }
        if meta.len() > MAX_FILE_BYTES {
            scan.warnings.push(format!("{name}：超过 2 MiB，已跳过"));
            scan.skipped.insert(name);
            continue;
        }
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                scan.warnings.push(format!("{name}：读取失败（{error}）"));
                scan.skipped.insert(name);
                continue;
            }
        };
        let text = match String::from_utf8(bytes.clone()) {
            Ok(text) => text,
            Err(_) => {
                scan.warnings.push(format!("{name}：不是 UTF-8，已跳过"));
                scan.skipped.insert(name);
                continue;
            }
        };

        scan.files.insert(
            name,
            LocalFile {
                sha256: sha256_base64(&bytes),
                bytes,
                text,
            },
        );
    }

    Ok(scan)
}

pub(crate) struct SyncEngine<B: SyncBackend> {
    backend: B,
    store: StateStore,
    notes_dir: PathBuf,
    device_slug: String,
    state: SyncState,
    state_warning: Option<String>,
    /// 状态文件读不出来时记在这里；本轮直接中止，不用空状态覆盖原文件。
    state_error: Option<String>,
    io: Arc<Mutex<()>>,
}

impl<B: SyncBackend> SyncEngine<B> {
    pub(crate) fn new(
        backend: B,
        store: StateStore,
        notes_dir: PathBuf,
        device_name: &str,
        io: Arc<Mutex<()>>,
    ) -> Self {
        let (state, state_warning, state_error) = match store.load() {
            Ok((state, warning)) => (state, warning, None),
            Err(error) => (
                SyncState::default(),
                None,
                Some(format!("读取同步状态失败：{error}")),
            ),
        };
        Self {
            backend,
            store,
            notes_dir,
            device_slug: device_slug(device_name),
            state,
            state_warning,
            state_error,
            io,
        }
    }

    /// 跑一轮同步。除「目录不可用」外不返回 Err：单个文件的失败记在汇总里，
    /// 下一轮会重试。
    pub(crate) async fn run(&mut self) -> SyncSummary {
        let mut summary = SyncSummary::new();
        if let Some(error) = self.state_error.take() {
            // 状态暂时读不出来：整轮不动，更不能拿空状态去覆盖原文件。
            summary.aborted = true;
            summary.error("同步状态", error);
            summary.finished_at = Some(now_iso());
            return summary;
        }
        if let Some(warning) = self.state_warning.take() {
            summary.warnings.push(warning);
        }

        let notes_dir = self.notes_dir.clone();
        let scan = match blocking(move || scan_local(&notes_dir)).await {
            Ok(Ok(scan)) => scan,
            Ok(Err(error)) => {
                summary.aborted = true;
                summary.error("笔记目录", error);
                summary.finished_at = Some(now_iso());
                return summary;
            }
            Err(error) => {
                summary.aborted = true;
                summary.error("笔记目录", error);
                summary.finished_at = Some(now_iso());
                return summary;
            }
        };
        summary.warnings.extend(scan.warnings.iter().cloned());

        let caps = self.backend.capabilities();
        let remote_list = match self.backend.list().await {
            Ok(list) => list,
            Err(error) => {
                summary.aborted = true;
                summary.error("远端列表", error);
                summary.finished_at = Some(now_iso());
                return summary;
            }
        };

        let mut remote: BTreeMap<String, RemoteMeta> = BTreeMap::new();
        for meta in remote_list {
            if is_sync_name(&meta.name) {
                remote.insert(meta.name.clone(), meta);
            } else {
                summary
                    .warnings
                    .push(format!("远端文件名不合法，已跳过：{}", meta.name));
            }
        }

        let mut names: BTreeSet<String> = BTreeSet::new();
        names.extend(scan.files.keys().cloned());
        names.extend(remote.keys().cloned());
        names.extend(self.state.files.keys().cloned());

        let mut plans: Vec<(String, Action)> = Vec::new();
        for name in names {
            if scan.skipped.contains(&name) {
                continue;
            }
            let record = self.state.files.get(&name);
            let local = match (scan.files.get(&name), record) {
                (Some(file), Some(record)) if file.sha256 == record.local_sha256 => {
                    LocalState::Unchanged
                }
                (Some(_), _) => LocalState::Changed,
                (None, _) => LocalState::Absent,
            };
            let remote_present = remote.contains_key(&name);
            let remote_changed = match (remote_present, record) {
                (true, Some(record)) => remote[&name].version != record.remote_version,
                (true, None) => true,
                (false, _) => false,
            };
            plans.push((
                name,
                decide(
                    &caps,
                    record.is_some(),
                    local,
                    remote_present,
                    remote_changed,
                ),
            ));
        }

        // 删除安全阀：先在计划层面拦住，误删的代价远高于少同步一轮。
        let deletions = plans
            .iter()
            .filter(|(_, action)| matches!(action, Action::DeleteRemote | Action::TrashLocal))
            .count();
        let tracked = self.state.files.len();
        let mass_deletion = deletions > DELETE_MIN_FILES
            && deletions * 100 > tracked.saturating_mul(DELETE_RATIO_PERCENT);
        // 「本地消失」只算本地真的没了的已跟踪文件：TrashLocal 是远端删除传播到
        // 本地，本地内容还在，不属于「目录被清空」。Forget 也算，避免远端刚好已删
        // 掉一两篇时差几个数漏判。
        let local_gone = plans
            .iter()
            .filter(|(_, action)| matches!(action, Action::DeleteRemote | Action::Forget))
            .count();
        let remote_deletes = plans
            .iter()
            .filter(|(_, action)| matches!(action, Action::DeleteRemote))
            .count();
        let all_missing =
            tracked >= DELETE_ALL_MIN_FILES && remote_deletes > 0 && local_gone == tracked;
        if mass_deletion || all_missing {
            summary.aborted = true;
            summary.error(
                "安全阀",
                format!("本轮拟删除 {deletions} 个文件（共 {tracked} 个已同步），已中止"),
            );
            summary.finished_at = Some(now_iso());
            return summary;
        }

        let mut trash_dir: Option<PathBuf> = None;
        for (name, action) in plans {
            let local = scan.files.get(&name);
            let record = self.state.files.get(&name).cloned();
            let remote_meta = remote.get(&name).cloned();
            self.apply(
                &name,
                action,
                local,
                remote_meta.as_ref(),
                record,
                &mut trash_dir,
                &mut summary,
            )
            .await;
        }

        self.state.last_sync_at = Some(now_iso());
        let store = self.store.clone();
        let snapshot = self.state.clone();
        match blocking(move || store.save(&snapshot)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => summary.error("同步状态", error),
            Err(error) => summary.error("同步状态", error),
        }
        summary.finished_at = Some(now_iso());
        summary
    }

    /// 懒创建本轮 trash 目录：同一轮的所有删除共用一个时间戳目录。
    async fn ensure_trash_dir(&self, slot: &mut Option<PathBuf>) -> Result<PathBuf, String> {
        if let Some(dir) = slot.as_ref() {
            return Ok(dir.clone());
        }
        let store = self.store.clone();
        match blocking(move || store.new_trash_dir()).await {
            Ok(Ok(dir)) => {
                *slot = Some(dir.clone());
                Ok(dir)
            }
            Ok(Err(error)) => Err(format!("创建 trash 目录失败：{error}")),
            Err(error) => Err(error.to_string()),
        }
    }

    async fn backup_to_trash(
        &self,
        dir: PathBuf,
        name: &str,
        bytes: Vec<u8>,
    ) -> Result<(), String> {
        let store = self.store.clone();
        let owned_name = name.to_string();
        match blocking(move || store.trash(&dir, &owned_name, &bytes)).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(error)) => Err(format!("备份到 trash 失败：{error}")),
            Err(error) => Err(error.to_string()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn apply(
        &mut self,
        name: &str,
        action: Action,
        local: Option<&LocalFile>,
        remote: Option<&RemoteMeta>,
        record: Option<FileRecord>,
        trash_dir: &mut Option<PathBuf>,
        summary: &mut SyncSummary,
    ) {
        // 名字可能来自远端列表或状态文件，落盘前先过白名单：这是路径穿越的最后一道闸。
        if !is_valid_note_name(name) {
            summary
                .warnings
                .push(format!("{name}：文件名不合法，已跳过"));
            return;
        }

        // 远端文件超限时直接跳过：既不下载，也不推断删除。
        let reads_remote = matches!(
            action,
            Action::Download | Action::RestoreLocal | Action::Merge
        );
        if reads_remote && remote.is_some_and(|meta| meta.size > MAX_FILE_BYTES) {
            summary
                .warnings
                .push(format!("{name}：远端文件超过 2 MiB，已跳过"));
            return;
        }

        match action {
            Action::Skip => summary.skipped += 1,
            Action::Upload | Action::UploadRevive => {
                let Some(local) = local else {
                    summary.skipped += 1;
                    return;
                };
                // 列表可能漏掉远端文件；复活写入前确认一次，避免覆盖另一台设备。
                if action == Action::UploadRevive {
                    match self.backend.exists(name).await {
                        Ok(true) => {
                            summary
                                .warnings
                                .push(format!("{name}：远端仍存在，本轮不覆盖"));
                            return;
                        }
                        Ok(false) => {}
                        Err(error @ SyncError::Budget) => {
                            summary.warnings.push(format!("{name}：{error}"));
                            return;
                        }
                        Err(error) => {
                            summary.error(name, format!("无法确认远端状态，未上传：{error}"));
                            return;
                        }
                    }
                }
                let expect = match (action, &record, remote) {
                    (Action::Upload, Some(record), Some(_)) => Expect::Version {
                        version: record.remote_version.clone(),
                        sha256: Some(record.remote_sha256.clone()),
                    },
                    _ => Expect::Absent,
                };
                match self.backend.put(name, &local.bytes, expect).await {
                    Ok(version) => {
                        self.commit(name, &local.bytes, version, summary).await;
                        summary.uploaded += 1;
                    }
                    Err(error) => summary.record(name, error),
                }
            }
            Action::Download | Action::RestoreLocal => match self.backend.get(name).await {
                Ok(Some((bytes, version))) => {
                    // 列表里的 size 可能缺失或被服务器写错，收到内容后再复查一次。
                    if bytes.len() as u64 > MAX_FILE_BYTES {
                        summary
                            .warnings
                            .push(format!("{name}：远端文件超过 2 MiB，已跳过"));
                        return;
                    }
                    // 与扫描、合并使用同一口径：不是 UTF-8 的内容绝不写进本地，
                    // 否则下一轮会因为无法解析而永远跳过这个文件。
                    if std::str::from_utf8(&bytes).is_err() {
                        summary
                            .warnings
                            .push(format!("{name}：远端文件不是 UTF-8，已跳过"));
                        return;
                    }
                    // 版本令牌可能来自 GET 与 PROPFIND 两个来源。内容与我们上次
                    // 同步的完全一致时，只把列表版本补进记录，避免反复下载。
                    if record
                        .as_ref()
                        .is_some_and(|record| sha256_base64(&bytes) == record.remote_sha256)
                    {
                        let version = remote.map(|meta| meta.version.clone()).unwrap_or(version);
                        self.commit(name, &bytes, version, summary).await;
                        summary.skipped += 1;
                        return;
                    }
                    // Download 的本地文件必须仍是扫描时那一份；RestoreLocal 则要求
                    // 它仍未出现。两种情况都不能覆盖网络阶段里用户新写的笔记。
                    let expect = match local {
                        Some(file) => LocalExpect::Sha(file.sha256.clone()),
                        None => LocalExpect::Absent,
                    };
                    if let Err(error) = self.write_local(name, bytes.clone(), expect).await {
                        summary.error(name, error);
                        return;
                    }
                    // 优先记录列表里的版本：下一轮 remote_changed 比较的正是列表版本，
                    // 而 GET/PUT 的版本来源不一定相同。
                    let version = remote.map(|meta| meta.version.clone()).unwrap_or(version);
                    self.commit(name, &bytes, version, summary).await;
                    summary.downloaded += 1;
                }
                Ok(None) => {
                    summary
                        .warnings
                        .push(format!("{name}：远端文件已消失，留待下一轮"));
                }
                Err(error) => summary.record(name, error),
            },
            Action::Merge => {
                let Some(local) = local else {
                    summary.skipped += 1;
                    return;
                };
                let (remote_bytes, remote_version) = match self.backend.get(name).await {
                    Ok(Some(value)) => value,
                    Ok(None) => {
                        summary
                            .warnings
                            .push(format!("{name}：远端文件已消失，留待下一轮"));
                        return;
                    }
                    Err(error) => {
                        summary.record(name, error);
                        return;
                    }
                };
                if remote_bytes.len() as u64 > MAX_FILE_BYTES {
                    summary
                        .warnings
                        .push(format!("{name}：远端文件超过 2 MiB，已跳过"));
                    return;
                }
                let Some(remote_text) = String::from_utf8(remote_bytes).ok() else {
                    summary.error(name, "远端文件不是 UTF-8，已跳过");
                    return;
                };
                let store = self.store.clone();
                let base_name = name.to_string();
                // 「没有 base」与「base 读不出来」必须分开：前者是首次合并，按并集
                // 处理；后者若也当空 base，会把本地删除当成新增而复活条目。
                let base = match blocking(move || store.read_base(&base_name)).await {
                    Ok(Ok(None)) => String::new(),
                    Ok(Ok(Some(bytes))) => match String::from_utf8(bytes) {
                        Ok(text) => text,
                        Err(_) => {
                            summary.error(name, "base 副本不是 UTF-8，已跳过本次合并");
                            return;
                        }
                    },
                    Ok(Err(error)) => {
                        summary.error(name, format!("读取 base 副本失败，已跳过本次合并：{error}"));
                        return;
                    }
                    Err(error) => {
                        summary.error(name, error);
                        return;
                    }
                };

                let local_text = local.text.clone();
                let remote_for_merge = remote_text.clone();
                let merged =
                    blocking(move || merge::merge(&base, &local_text, &remote_for_merge)).await;
                match merged {
                    Ok(Some(result)) => {
                        if result.text != local.text {
                            if let Err(error) = self
                                .write_local(
                                    name,
                                    result.text.as_bytes().to_vec(),
                                    LocalExpect::Sha(local.sha256.clone()),
                                )
                                .await
                            {
                                summary.error(name, error);
                                return;
                            }
                        }
                        match self
                            .backend
                            .put(
                                name,
                                result.text.as_bytes(),
                                Expect::Version {
                                    version: remote_version,
                                    sha256: Some(sha256_base64(remote_text.as_bytes())),
                                },
                            )
                            .await
                        {
                            Ok(version) => {
                                self.commit(name, result.text.as_bytes(), version, summary)
                                    .await;
                                summary.merged += 1;
                                summary.conflicts += result.conflicts;
                            }
                            Err(error) => summary.record(name, error),
                        }
                    }
                    Ok(None) => {
                        self.conflict_copy(name, local, &remote_text, remote_version, summary)
                            .await
                    }
                    Err(error) => summary.error(name, error),
                }
            }
            Action::DeleteRemote => {
                let Some(record) = record else {
                    return;
                };
                // 本地文件已经由用户删除，远端这一份是最后可恢复的内容。删之前先
                // 备份到 trash，否则笔记目录被清空或网络盘没挂上时两端都不留原文。
                let store = self.store.clone();
                let owned_name = name.to_string();
                let base = match blocking(move || store.read_base(&owned_name)).await {
                    Ok(Ok(base)) => base,
                    Ok(Err(error)) => {
                        summary.error(name, format!("读取 base 副本失败：{error}"));
                        return;
                    }
                    Err(error) => {
                        summary.error(name, error);
                        return;
                    }
                };
                let backup = match base {
                    // base 可能是一次 `write_base` 失败后留下的旧版本；摘要对不上就
                    // 重新读远端，不能把旧内容当成「最后可恢复副本」。
                    Some(bytes) if sha256_base64(&bytes) == record.remote_sha256 => bytes,
                    _ => match self.backend.get(name).await {
                        Ok(Some((bytes, _))) => bytes,
                        Ok(None) => {
                            // 远端已经不在，等同于删除成功。
                            self.forget(name).await;
                            summary.deleted += 1;
                            return;
                        }
                        Err(error) => {
                            summary.record(name, error);
                            return;
                        }
                    },
                };
                let dir = match self.ensure_trash_dir(trash_dir).await {
                    Ok(dir) => dir,
                    Err(error) => {
                        summary.error(name, error);
                        return;
                    }
                };
                if let Err(error) = self.backup_to_trash(dir, name, backup).await {
                    summary.error(name, error);
                    return;
                }
                match self
                    .backend
                    .delete(
                        name,
                        Expect::Version {
                            version: record.remote_version,
                            sha256: Some(record.remote_sha256),
                        },
                    )
                    .await
                {
                    Ok(()) => {
                        self.forget(name).await;
                        summary.deleted += 1;
                    }
                    Err(SyncError::NotFound) => {
                        self.forget(name).await;
                        summary.deleted += 1;
                    }
                    Err(error) => summary.record(name, error),
                }
            }
            Action::TrashLocal => {
                let Some(local) = local else {
                    return;
                };
                // 目录列表可能被服务器或代理截断。删除本地副本前先直接确认一次
                // 远端确实不在了；确认失败就本轮不动，宁可少同步一次。
                match self.backend.exists(name).await {
                    Ok(false) => {}
                    Ok(true) => {
                        summary
                            .warnings
                            .push(format!("{name}：远端仍存在，本轮不删除本地副本"));
                        return;
                    }
                    Err(error @ SyncError::Budget) => {
                        summary.warnings.push(format!("{name}：{error}"));
                        return;
                    }
                    Err(error) => {
                        summary.error(name, format!("无法确认远端状态，未删除本地副本：{error}"));
                        return;
                    }
                }
                let dir = match self.ensure_trash_dir(trash_dir).await {
                    Ok(dir) => dir,
                    Err(error) => {
                        summary.error(name, error);
                        return;
                    }
                };
                if let Err(error) = self.backup_to_trash(dir, name, local.bytes.clone()).await {
                    summary.error(name, error);
                    return;
                }
                if let Err(error) = self.remove_local(name, Some(local.sha256.clone())).await {
                    summary.error(name, error);
                    return;
                }
                self.forget(name).await;
                summary.deleted += 1;
            }
            Action::Forget => self.forget(name).await,
        }
    }

    /// 合并失败：远端内容另存为冲突副本，本地原文照常上传，绝不整文件覆盖。
    async fn conflict_copy(
        &mut self,
        name: &str,
        local: &LocalFile,
        remote_text: &str,
        remote_version: RemoteVersion,
        summary: &mut SyncSummary,
    ) {
        let Some(conflict) = conflict_name(name, &self.device_slug) else {
            summary.error(name, "无法生成冲突副本名");
            return;
        };
        if let Err(error) = self
            .write_local(&conflict, remote_text.as_bytes().to_vec(), LocalExpect::Any)
            .await
        {
            summary.error(name, error);
            return;
        }
        match self
            .backend
            .put(
                name,
                &local.bytes,
                Expect::Version {
                    version: remote_version,
                    sha256: Some(sha256_base64(remote_text.as_bytes())),
                },
            )
            .await
        {
            Ok(version) => {
                self.commit(name, &local.bytes, version, summary).await;
                summary.conflicts += 1;
                summary
                    .warnings
                    .push(format!("{name}：无法逐条合并，远端内容已保存为 {conflict}"));
            }
            Err(error) => summary.record(name, error),
        }
    }

    async fn commit(
        &mut self,
        name: &str,
        bytes: &[u8],
        version: RemoteVersion,
        summary: &mut SyncSummary,
    ) {
        let notes_dir = self.notes_dir.clone();
        let store = self.store.clone();
        let owned_name = name.to_string();
        let owned_bytes = bytes.to_vec();
        let prepared = blocking(move || {
            let path = notes_dir.join(&owned_name);
            let (size, modified) = match fs::metadata(&path) {
                Ok(meta) => (meta.len(), mtime_ns(meta.modified().unwrap_or(UNIX_EPOCH))),
                Err(_) => (owned_bytes.len() as u64, 0),
            };
            let sha = sha256_base64(&owned_bytes);
            let base_result = store.write_base(&owned_name, &owned_bytes);
            let record = FileRecord {
                local_mtime_ns: modified,
                local_size: size,
                local_sha256: sha.clone(),
                remote_version: version,
                remote_sha256: sha,
                synced_at: now_iso(),
            };
            (record, base_result)
        })
        .await;

        let (record, base_result) = match prepared {
            Ok(value) => value,
            Err(error) => {
                summary.error(name, error);
                return;
            }
        };
        if let Err(error) = base_result {
            // 记录必须与 base 同源：base 没写成功就不能推进状态，否则下一轮会拿
            // 陈旧 base 做三方合并，把已删除的条目复活。
            summary.error(name, format!("保存 base 副本失败，本轮不记录：{error}"));
            return;
        }
        self.state.files.insert(name.to_string(), record);

        let store = self.store.clone();
        let snapshot = self.state.clone();
        match blocking(move || store.save(&snapshot)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => summary.error(name, format!("保存同步状态失败：{error}")),
            Err(error) => summary.error(name, error),
        }
    }

    async fn forget(&mut self, name: &str) {
        self.state.files.remove(name);
        let store = self.store.clone();
        let owned_name = name.to_string();
        let snapshot = self.state.clone();
        let _ = blocking(move || {
            store.remove_base(&owned_name);
            let _ = store.save(&snapshot);
        })
        .await;
    }

    async fn write_local(
        &self,
        name: &str,
        bytes: Vec<u8>,
        expect: LocalExpect,
    ) -> Result<(), SyncError> {
        let notes_dir = self.notes_dir.clone();
        let io = self.io.clone();
        let owned_name = name.to_string();
        blocking(move || {
            if !is_valid_note_name(&owned_name) {
                return Err(SyncError::Other(format!("文件名不合法：{owned_name}")));
            }
            let path = notes_dir.join(&owned_name);
            {
                let _guard = io.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                match expect {
                    LocalExpect::Absent => {
                        if path.exists() {
                            return Err(SyncError::Other(format!(
                                "{owned_name}：本地文件在同步期间出现，已跳过写入"
                            )));
                        }
                    }
                    LocalExpect::Sha(expected) => {
                        let current = fs::read(&path).map_err(|error| {
                            SyncError::Io(format!("读取 {owned_name} 失败：{error}"))
                        })?;
                        if sha256_base64(&current) != expected {
                            return Err(SyncError::Other(format!(
                                "{owned_name}：本地文件在同步期间被修改，已跳过写入"
                            )));
                        }
                    }
                    LocalExpect::Any => {}
                }
                write_atomic(&path, &bytes)
                    .map_err(|error| SyncError::Io(format!("写入 {owned_name} 失败：{error}")))?;
            }
            notes::invalidate_cache(&path);
            Ok(())
        })
        .await?
    }

    async fn remove_local(&self, name: &str, expected: Option<String>) -> Result<(), SyncError> {
        let notes_dir = self.notes_dir.clone();
        let io = self.io.clone();
        let owned_name = name.to_string();
        blocking(move || {
            if !is_valid_note_name(&owned_name) {
                return Err(SyncError::Other(format!("文件名不合法：{owned_name}")));
            }
            let path = notes_dir.join(&owned_name);
            {
                let _guard = io.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(expected) = expected {
                    let current = fs::read(&path).map_err(|error| {
                        SyncError::Io(format!("读取 {owned_name} 失败：{error}"))
                    })?;
                    if sha256_base64(&current) != expected {
                        return Err(SyncError::Other(format!(
                            "{owned_name}：本地文件在同步期间被修改，已取消删除"
                        )));
                    }
                }
                fs::remove_file(&path)
                    .map_err(|error| SyncError::Io(format!("删除 {owned_name} 失败：{error}")))?;
            }
            notes::invalidate_cache(&path);
            Ok(())
        })
        .await?
    }
}

fn device_slug(name: &str) -> String {
    let mut slug = String::new();
    for ch in name.chars() {
        let ch = ch.to_ascii_lowercase();
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
        if slug.len() >= 16 {
            break;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "device".to_string()
    } else {
        slug
    }
}

/// `<日期>.conflict-<设备>-<时间>.md`，符合协议里的文件名规则。
fn conflict_name(name: &str, slug: &str) -> Option<String> {
    let date = name.get(..10)?;
    if chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_err() {
        return None;
    }
    // 带毫秒：同一秒内生成两个冲突副本时不能互相覆盖。
    let stamp = Local::now().format("%Y%m%d%H%M%S%3f");
    Some(format!("{date}.conflict-{slug}-{stamp}.md"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::backend::SyncBackend;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    #[derive(Clone)]
    struct FakeBackend {
        inner: Arc<FakeInner>,
    }

    struct FakeInner {
        caps: Capabilities,
        files: Mutex<BTreeMap<String, (Vec<u8>, RemoteVersion)>>,
        next: AtomicU64,
        fail_list: AtomicBool,
        /// 模拟被截断的目录列表：文件仍在，但 list() 看不到它。
        hidden: Mutex<BTreeSet<String>>,
        /// 模拟 GET/PUT 与 PROPFIND 使用不同来源的版本令牌。
        diverge_list: AtomicBool,
    }

    impl FakeBackend {
        fn new(caps: Capabilities) -> Self {
            Self {
                inner: Arc::new(FakeInner {
                    caps,
                    files: Mutex::new(BTreeMap::new()),
                    next: AtomicU64::new(1),
                    fail_list: AtomicBool::new(false),
                    hidden: Mutex::new(BTreeSet::new()),
                    diverge_list: AtomicBool::new(false),
                }),
            }
        }

        fn strong() -> Self {
            Self::new(Capabilities {
                strong_etag: true,
                honors_if_match: true,
                honors_if_none_match: true,
                reliable_listing: true,
            })
        }

        fn content(&self, name: &str) -> Option<String> {
            self.inner
                .files
                .lock()
                .unwrap()
                .get(name)
                .map(|(bytes, _)| String::from_utf8(bytes.clone()).unwrap())
        }

        fn names(&self) -> Vec<String> {
            self.inner.files.lock().unwrap().keys().cloned().collect()
        }

        fn remove(&self, name: &str) {
            self.inner.files.lock().unwrap().remove(name);
        }

        /// 直接往「远端」放一个文件，模拟别的客户端上传。
        fn put_remote(&self, name: &str, bytes: Vec<u8>) {
            let version = RemoteVersion::Number(self.inner.next.fetch_add(1, Ordering::SeqCst));
            self.inner
                .files
                .lock()
                .unwrap()
                .insert(name.to_string(), (bytes, version));
        }

        fn set_fail_list(&self, value: bool) {
            self.inner.fail_list.store(value, Ordering::SeqCst);
        }

        /// 模拟「列表里看不到、但远端仍然存在」。
        fn hide_from_list(&self, name: &str) {
            self.inner.hidden.lock().unwrap().insert(name.to_string());
        }

        /// 模拟 GET/PUT 与 PROPFIND 使用不同来源的版本令牌。
        fn diverge_list_versions(&self) {
            self.inner.diverge_list.store(true, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl SyncBackend for FakeBackend {
        fn capabilities(&self) -> Capabilities {
            self.inner.caps.clone()
        }

        async fn list(&self) -> Result<Vec<RemoteMeta>, SyncError> {
            if self.inner.fail_list.load(Ordering::SeqCst) {
                return Err(SyncError::Network("列表失败".into()));
            }
            let hidden = self.inner.hidden.lock().unwrap().clone();
            Ok(self
                .inner
                .files
                .lock()
                .unwrap()
                .iter()
                .filter(|(name, _)| !hidden.contains(name.as_str()))
                .map(|(name, (bytes, version))| {
                    let version = if self.inner.diverge_list.load(Ordering::SeqCst) {
                        match version {
                            RemoteVersion::Number(number) => {
                                RemoteVersion::Hash(format!("list-{number}"))
                            }
                            other => other.clone(),
                        }
                    } else {
                        version.clone()
                    };
                    RemoteMeta {
                        name: name.clone(),
                        version,
                        size: bytes.len() as u64,
                    }
                })
                .collect())
        }

        async fn get(&self, name: &str) -> Result<Option<(Vec<u8>, RemoteVersion)>, SyncError> {
            Ok(self.inner.files.lock().unwrap().get(name).cloned())
        }

        async fn exists(&self, name: &str) -> Result<bool, SyncError> {
            Ok(self.inner.files.lock().unwrap().contains_key(name))
        }

        async fn put(
            &self,
            name: &str,
            data: &[u8],
            expect: Expect,
        ) -> Result<RemoteVersion, SyncError> {
            let mut files = self.inner.files.lock().unwrap();
            if self.inner.caps.honors_if_match {
                match &expect {
                    Expect::Absent if files.contains_key(name) => {
                        return Err(SyncError::VersionMismatch)
                    }
                    Expect::Version { version, sha256 } => match files.get(name) {
                        Some((bytes, current)) if current == version => {
                            if sha256
                                .as_deref()
                                .is_some_and(|expected| sha256_base64(bytes) != expected)
                            {
                                return Err(SyncError::VersionMismatch);
                            }
                        }
                        _ => return Err(SyncError::VersionMismatch),
                    },
                    _ => {}
                }
            }
            let version = RemoteVersion::Number(self.inner.next.fetch_add(1, Ordering::SeqCst));
            files.insert(name.to_string(), (data.to_vec(), version.clone()));
            Ok(version)
        }

        async fn delete(&self, name: &str, expect: Expect) -> Result<(), SyncError> {
            let mut files = self.inner.files.lock().unwrap();
            if self.inner.caps.honors_if_match {
                if let Expect::Version { version, sha256 } = &expect {
                    match files.get(name) {
                        Some((bytes, current)) if current == version => {
                            if sha256
                                .as_deref()
                                .is_some_and(|expected| sha256_base64(bytes) != expected)
                            {
                                return Err(SyncError::VersionMismatch);
                            }
                        }
                        _ => return Err(SyncError::VersionMismatch),
                    }
                }
            }
            files.remove(name).map(|_| ()).ok_or(SyncError::NotFound)
        }
    }

    struct Harness {
        root: PathBuf,
        notes: PathBuf,
        config: PathBuf,
        backend: FakeBackend,
    }

    impl Harness {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "flashnote-sync-engine-{name}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(root.join("notes")).unwrap();
            fs::create_dir_all(root.join("config")).unwrap();
            Self {
                notes: root.join("notes"),
                config: root.join("config"),
                backend: FakeBackend::strong(),
                root,
            }
        }

        fn with_backend(name: &str, backend: FakeBackend) -> Self {
            let mut harness = Self::new(name);
            harness.backend = backend;
            harness
        }

        fn engine(&self) -> SyncEngine<FakeBackend> {
            SyncEngine::new(
                self.backend.clone(),
                StateStore::new(&self.config, &self.notes),
                self.notes.clone(),
                "TEST-PC",
                Arc::new(Mutex::new(())),
            )
        }

        fn write(&self, name: &str, text: &str) {
            fs::write(self.notes.join(name), text).unwrap();
        }

        fn read(&self, name: &str) -> String {
            fs::read_to_string(self.notes.join(name)).unwrap()
        }

        fn run(&self) -> SyncSummary {
            tauri::async_runtime::block_on(self.engine().run())
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn decision_table_covers_all_rows() {
        let caps = Capabilities {
            strong_etag: true,
            honors_if_match: true,
            honors_if_none_match: true,
            reliable_listing: true,
        };
        assert_eq!(
            decide(&caps, false, LocalState::Changed, false, false),
            Action::Upload
        );
        assert_eq!(
            decide(&caps, false, LocalState::Changed, true, true),
            Action::Merge
        );
        assert_eq!(
            decide(&caps, false, LocalState::Absent, true, true),
            Action::Download
        );
        assert_eq!(
            decide(&caps, true, LocalState::Unchanged, true, false),
            Action::Skip
        );
        assert_eq!(
            decide(&caps, true, LocalState::Changed, true, false),
            Action::Upload
        );
        assert_eq!(
            decide(&caps, true, LocalState::Unchanged, true, true),
            Action::Download
        );
        assert_eq!(
            decide(&caps, true, LocalState::Changed, true, true),
            Action::Merge
        );
        assert_eq!(
            decide(&caps, true, LocalState::Absent, true, false),
            Action::DeleteRemote
        );
        assert_eq!(
            decide(&caps, true, LocalState::Absent, true, true),
            Action::RestoreLocal
        );
        assert_eq!(
            decide(&caps, true, LocalState::Unchanged, false, false),
            Action::TrashLocal
        );
        assert_eq!(
            decide(&caps, true, LocalState::Changed, false, false),
            Action::UploadRevive
        );
        assert_eq!(
            decide(&caps, true, LocalState::Absent, false, false),
            Action::Forget
        );
    }

    #[test]
    fn weak_server_only_uploads() {
        let weak = Capabilities {
            strong_etag: false,
            honors_if_match: false,
            honors_if_none_match: false,
            reliable_listing: true,
        };
        // 远端被删 → 把本地补回去，而不是删本地；上传前先 exists() 复核。
        assert_eq!(
            decide(&weak, true, LocalState::Unchanged, false, false),
            Action::UploadRevive
        );
        // 本地删除 → 无法传播；保留记录，避免下一轮重新下载。
        assert_eq!(
            decide(&weak, true, LocalState::Absent, true, false),
            Action::Skip
        );
        // 列表不可靠 → 远端缺失时不能推断删除；但本地有修改仍应尝试上传，
        // apply 会先向远端确认一次再决定是否覆盖。
        let blind = Capabilities {
            strong_etag: false,
            honors_if_match: false,
            honors_if_none_match: false,
            reliable_listing: false,
        };
        assert_eq!(
            decide(&blind, true, LocalState::Unchanged, false, false),
            Action::Skip
        );
        assert_eq!(
            decide(&blind, true, LocalState::Changed, false, false),
            Action::UploadRevive
        );
        // 没有记录时同样不能只凭不可靠的列表就认定远端为空。
        assert_eq!(
            decide(&blind, false, LocalState::Changed, false, false),
            Action::UploadRevive
        );
        assert_eq!(
            decide(&blind, false, LocalState::Changed, true, true),
            Action::Merge
        );
    }

    #[test]
    fn uploads_then_downloads_to_second_client() {
        let backend = FakeBackend::strong();
        let first = Harness::with_backend("first-upload", backend.clone());
        let second = Harness::with_backend("second-download", backend.clone());
        let text = "# 2026-10-23\n\n- 10:00 base\n";
        first.write("2026-10-23.md", text);

        let summary = first.run();
        assert_eq!(summary.uploaded, 1, "{summary:?}");
        assert_eq!(backend.content("2026-10-23.md").as_deref(), Some(text));

        let summary = second.run();
        assert_eq!(summary.downloaded, 1, "{summary:?}");
        assert_eq!(second.read("2026-10-23.md"), text);
    }

    #[test]
    fn version_token_mismatch_reconciles_without_rewriting() {
        let backend = FakeBackend::strong();
        let harness = Harness::with_backend("version-reconcile", backend.clone());
        let text = "# 2026-10-23\n\n- 10:00 a\n";
        harness.write("2026-10-23.md", text);
        assert_eq!(harness.run().uploaded, 1);

        // 列表开始返回与 GET/PUT 不同来源的版本令牌。
        backend.diverge_list_versions();
        let summary = harness.run();
        assert_eq!(summary.downloaded, 0, "{summary:?}");
        assert_eq!(summary.skipped, 1, "{summary:?}");
        assert_eq!(harness.read("2026-10-23.md"), text);

        // 记录已对齐列表版本，下一轮直接跳过。
        let summary = harness.run();
        assert_eq!(summary.downloaded, 0, "{summary:?}");
        assert_eq!(summary.skipped, 1, "{summary:?}");
    }

    #[test]
    fn concurrent_edits_converge_without_duplicates() {
        let backend = FakeBackend::strong();
        let first = Harness::with_backend("converge-a", backend.clone());
        let second = Harness::with_backend("converge-b", backend.clone());
        first.write("2026-10-23.md", "# 2026-10-23\n\n- 10:00 base\n");
        first.run();
        second.run();

        first.write(
            "2026-10-23.md",
            "# 2026-10-23\n\n- 10:00 base\n\n- 11:00 from-a\n",
        );
        second.write(
            "2026-10-23.md",
            "# 2026-10-23\n\n- 10:00 base\n\n- 12:00 from-b\n",
        );

        assert_eq!(first.run().uploaded, 1);
        let summary = second.run();
        assert_eq!(summary.merged, 1, "{summary:?}");
        first.run();

        for harness in [&first, &second] {
            let text = harness.read("2026-10-23.md");
            assert_eq!(text.matches("- 11:00 from-a").count(), 1, "{text}");
            assert_eq!(text.matches("- 12:00 from-b").count(), 1, "{text}");
            assert_eq!(text.matches("- 10:00 base").count(), 1, "{text}");
        }
        assert_eq!(
            backend.content("2026-10-23.md"),
            Some(first.read("2026-10-23.md"))
        );
    }

    #[test]
    fn safety_valve_blocks_mass_deletion() {
        let harness = Harness::new("valve");
        for day in 1..=10 {
            harness.write(
                &format!("2026-10-{day:02}.md"),
                &format!("# 2026-10-{day:02}\n"),
            );
        }
        assert_eq!(harness.run().uploaded, 10);

        for day in 1..=6 {
            fs::remove_file(harness.notes.join(format!("2026-10-{day:02}.md"))).unwrap();
        }
        let summary = harness.run();
        assert!(summary.aborted, "{summary:?}");
        assert!(summary.errors.iter().any(|error| error.contains("安全阀")));
        assert_eq!(harness.backend.names().len(), 10);

        // 恢复被删的文件，只删一个，安全阀不应再拦。
        for day in 1..=6 {
            harness.write(
                &format!("2026-10-{day:02}.md"),
                &format!("# 2026-10-{day:02}\n"),
            );
        }
        fs::remove_file(harness.notes.join("2026-10-07.md")).unwrap();
        let summary = harness.run();
        assert!(!summary.aborted, "{summary:?}");
        assert_eq!(summary.deleted, 1);
        assert_eq!(harness.backend.names().len(), 9);
    }

    #[test]
    fn all_tracked_files_missing_aborts_small_library() {
        let harness = Harness::new("valve-small");
        for day in 1..=4 {
            harness.write(
                &format!("2026-10-{day:02}.md"),
                &format!("# 2026-10-{day:02}\n"),
            );
        }
        assert_eq!(harness.run().uploaded, 4);

        // 目录还在但内容全没了：小笔记库也必须拦住。
        for day in 1..=4 {
            fs::remove_file(harness.notes.join(format!("2026-10-{day:02}.md"))).unwrap();
        }
        let summary = harness.run();
        assert!(summary.aborted, "{summary:?}");
        assert!(summary.errors.iter().any(|error| error.contains("安全阀")));
        assert_eq!(harness.backend.names().len(), 4);
    }

    #[test]
    fn remote_cleared_trashes_local_instead_of_aborting() {
        let harness = Harness::new("valve-trash-local");
        for day in 1..=4 {
            harness.write(
                &format!("2026-10-{day:02}.md"),
                &format!("# 2026-10-{day:02}\n"),
            );
        }
        assert_eq!(harness.run().uploaded, 4);

        // 另一台设备把远端删光：这是正常传播，应该把本地移进 trash，而不是中止。
        for name in harness.backend.names() {
            harness.backend.remove(&name);
        }
        let summary = harness.run();
        assert!(!summary.aborted, "{summary:?}");
        assert_eq!(summary.deleted, 4);
        for day in 1..=4 {
            assert!(!harness.notes.join(format!("2026-10-{day:02}.md")).exists());
        }
    }

    #[test]
    fn all_local_files_gone_with_one_already_missing_remotely_still_aborts() {
        let harness = Harness::new("valve-mixed");
        for day in 1..=4 {
            harness.write(
                &format!("2026-10-{day:02}.md"),
                &format!("# 2026-10-{day:02}\n"),
            );
        }
        harness.run();
        // 远端刚好已被别的设备删掉一篇：它走 Forget，不能因此漏判「本地全没了」。
        harness.backend.remove("2026-10-01.md");
        for day in 1..=4 {
            fs::remove_file(harness.notes.join(format!("2026-10-{day:02}.md"))).unwrap();
        }

        let summary = harness.run();
        assert!(summary.aborted, "{summary:?}");
        assert_eq!(harness.backend.names().len(), 3);
    }

    #[test]
    fn local_deletion_backs_up_remote_copy_before_delete() {
        let harness = Harness::new("delete-backup");
        harness.write("2026-10-23.md", "# 2026-10-23\n\n- 10:00 a\n");
        harness.run();
        fs::remove_file(harness.notes.join("2026-10-23.md")).unwrap();

        let summary = harness.run();
        assert_eq!(summary.deleted, 1, "{summary:?}");
        assert!(harness.backend.names().is_empty());

        let state_root = StateStore::new(&harness.config, &harness.notes)
            .root()
            .to_path_buf();
        let mut backups = Vec::new();
        for dir in fs::read_dir(state_root.join("trash")).unwrap().flatten() {
            for file in fs::read_dir(dir.path()).unwrap().flatten() {
                let path = file.path();
                backups.push((
                    path.file_name().unwrap().to_string_lossy().into_owned(),
                    fs::read_to_string(&path).unwrap_or_default(),
                ));
            }
        }
        assert!(
            backups
                .iter()
                .any(|(name, text)| name == "2026-10-23.md" && text.contains("10:00 a")),
            "删除远端前应留下可恢复副本：{backups:?}"
        );
    }

    #[test]
    fn deletion_uses_remote_content_when_base_is_stale() {
        let harness = Harness::new("delete-stale-base");
        harness.write("2026-10-23.md", "# 2026-10-23\n\n- 10:00 a\n");
        harness.run();
        let state_root = StateStore::new(&harness.config, &harness.notes)
            .root()
            .to_path_buf();
        // 模拟 write_base 失败后留下的旧副本。
        fs::write(state_root.join("base").join("2026-10-23.md"), "stale\n").unwrap();
        fs::remove_file(harness.notes.join("2026-10-23.md")).unwrap();

        let summary = harness.run();
        assert_eq!(summary.deleted, 1, "{summary:?}");
        let mut backups = Vec::new();
        for dir in fs::read_dir(state_root.join("trash")).unwrap().flatten() {
            for file in fs::read_dir(dir.path()).unwrap().flatten() {
                backups.push(fs::read_to_string(file.path()).unwrap_or_default());
            }
        }
        assert!(
            backups.iter().any(|text| text.contains("10:00 a")),
            "应备份远端内容而不是陈旧 base：{backups:?}"
        );
        assert!(
            !backups.iter().any(|text| text.contains("stale")),
            "陈旧 base 不能被当成可恢复副本：{backups:?}"
        );
    }

    #[test]
    fn remote_deletion_moves_local_file_to_trash() {
        let harness = Harness::new("remote-delete");
        harness.write("2026-10-23.md", "# 2026-10-23\n\n- 10:00 a\n");
        harness.run();
        harness.backend.remove("2026-10-23.md");

        let summary = harness.run();
        assert_eq!(summary.deleted, 1, "{summary:?}");
        assert!(!harness.notes.join("2026-10-23.md").exists());

        let trash = StateStore::new(&harness.config, &harness.notes)
            .root()
            .join("trash");
        let mut found = false;
        for dir in fs::read_dir(trash).unwrap().flatten() {
            if dir.path().join("2026-10-23.md").exists() {
                found = true;
            }
        }
        assert!(found, "本地副本应进入 trash");
    }

    #[test]
    fn weak_server_restores_remote_instead_of_deleting_local() {
        let backend = FakeBackend::new(Capabilities {
            strong_etag: false,
            honors_if_match: false,
            honors_if_none_match: false,
            reliable_listing: true,
        });
        let harness = Harness::with_backend("weak-restore", backend.clone());
        harness.write("2026-10-23.md", "# 2026-10-23\n\n- 10:00 a\n");
        harness.run();
        backend.remove("2026-10-23.md");

        let summary = harness.run();
        assert_eq!(summary.uploaded, 1, "{summary:?}");
        assert!(harness.notes.join("2026-10-23.md").exists());
        assert!(backend.content("2026-10-23.md").is_some());
    }

    #[test]
    fn weak_server_keeps_local_deletion_without_resurrection() {
        let backend = FakeBackend::new(Capabilities {
            strong_etag: false,
            honors_if_match: false,
            honors_if_none_match: false,
            reliable_listing: true,
        });
        let harness = Harness::with_backend("weak-delete", backend.clone());
        harness.write("2026-10-23.md", "# 2026-10-23\n\n- 10:00 a\n");
        assert_eq!(harness.run().uploaded, 1);

        fs::remove_file(harness.notes.join("2026-10-23.md")).unwrap();
        let summary = harness.run();
        assert_eq!(summary.deleted, 0, "{summary:?}");
        assert_eq!(summary.skipped, 1, "{summary:?}");
        assert!(!harness.notes.join("2026-10-23.md").exists());
        assert!(backend.content("2026-10-23.md").is_some());

        // 下一轮也不能把远端副本当成新文件下载回来。
        let summary = harness.run();
        assert_eq!(summary.downloaded, 0, "{summary:?}");
        assert!(!harness.notes.join("2026-10-23.md").exists());
    }

    #[test]
    fn invalid_utf8_is_skipped_without_deleting_remote() {
        let harness = Harness::new("invalid-utf8");
        harness.write("2026-10-23.md", "# 2026-10-23\n\n- 10:00 a\n");
        harness.run();
        fs::write(harness.notes.join("2026-10-23.md"), [0xff, 0xfe, 0x00]).unwrap();

        let summary = harness.run();
        assert_eq!(summary.uploaded, 0, "{summary:?}");
        assert!(summary
            .warnings
            .iter()
            .any(|warning| warning.contains("UTF-8")));
        assert!(harness.backend.content("2026-10-23.md").is_some());
    }

    #[test]
    fn oversized_remote_file_is_skipped() {
        let harness = Harness::new("oversize-remote");
        harness
            .backend
            .put_remote("2026-10-23.md", vec![b'a'; MAX_FILE_BYTES as usize + 1]);

        let summary = harness.run();
        assert_eq!(summary.downloaded, 0, "{summary:?}");
        assert!(summary
            .warnings
            .iter()
            .any(|warning| warning.contains("2 MiB")));
        assert!(!harness.notes.join("2026-10-23.md").exists());
        assert!(
            harness.backend.content("2026-10-23.md").is_some(),
            "不能删远端"
        );
    }

    #[test]
    fn non_utf8_remote_file_is_not_written_locally() {
        let backend = FakeBackend::strong();
        backend.put_remote("2026-10-23.md", vec![0xff, 0xfe, 0x00]);
        let harness = Harness::with_backend("non-utf8-download", backend);

        let summary = harness.run();
        assert_eq!(summary.downloaded, 0, "{summary:?}");
        assert!(summary
            .warnings
            .iter()
            .any(|warning| warning.contains("不是 UTF-8")));
        assert!(!harness.notes.join("2026-10-23.md").exists());
    }

    #[test]
    fn unreadable_state_aborts_instead_of_overwriting() {
        let harness = Harness::new("state-unreadable");
        harness.write("2026-10-23.md", "# 2026-10-23\n\n- 10:00 a\n");
        let state_root = StateStore::new(&harness.config, &harness.notes)
            .root()
            .to_path_buf();
        // 用目录占住 state.json：读取得到 NotFound 以外的错误。
        fs::create_dir_all(state_root.join("state.json")).unwrap();

        let summary = harness.run();
        assert!(summary.aborted, "{summary:?}");
        assert!(summary
            .errors
            .iter()
            .any(|error| error.contains("同步状态")));
        assert!(harness.backend.names().is_empty());
    }

    #[test]
    fn unreadable_base_skips_merge_instead_of_reviving_entries() {
        let harness = Harness::new("base-unreadable");
        harness.write("2026-10-23.md", "# 2026-10-23\n\n- 10:00 a\n");
        harness.run();

        let state_root = StateStore::new(&harness.config, &harness.notes)
            .root()
            .to_path_buf();
        let base = state_root.join("base").join("2026-10-23.md");
        fs::remove_file(&base).unwrap();
        fs::create_dir_all(&base).unwrap();
        harness.write("2026-10-23.md", "# 2026-10-23\n\n- 10:00 a\n\n- 11:00 b\n");
        harness.backend.put_remote(
            "2026-10-23.md",
            b"# 2026-10-23\n\n- 10:00 a\n\n- 12:00 c\n".to_vec(),
        );

        let summary = harness.run();
        assert!(
            summary
                .errors
                .iter()
                .any(|error| error.contains("读取 base 副本失败")),
            "{summary:?}"
        );
        assert_eq!(
            harness.read("2026-10-23.md"),
            "# 2026-10-23\n\n- 10:00 a\n\n- 11:00 b\n",
            "base 读不出来时不能拿空 base 合并"
        );
    }

    #[test]
    fn base_write_failure_does_not_advance_state() {
        let harness = Harness::new("base-write-fail");
        harness.write("2026-10-23.md", "# 2026-10-23\n\n- 10:00 a\n");
        let state_root = StateStore::new(&harness.config, &harness.notes)
            .root()
            .to_path_buf();
        // 让 base 的 rename 目标变成目录，写入必然失败。
        fs::create_dir_all(state_root.join("base").join("2026-10-23.md")).unwrap();

        let summary = harness.run();
        assert_eq!(summary.uploaded, 1, "{summary:?}");
        assert!(
            summary
                .errors
                .iter()
                .any(|error| error.contains("保存 base 副本失败")),
            "{summary:?}"
        );

        // 记录没有推进：下一轮不能把远端内容当成删除，也不能丢本地修改。
        let summary = harness.run();
        assert_eq!(summary.deleted, 0, "{summary:?}");
        assert!(harness.notes.join("2026-10-23.md").exists());
    }

    #[test]
    fn list_failure_aborts_without_touching_files() {
        let harness = Harness::new("list-failure");
        harness.write("2026-10-23.md", "# 2026-10-23\n\n- 10:00 a\n");
        harness.run();
        harness.backend.set_fail_list(true);

        let summary = harness.run();
        assert!(summary.aborted, "{summary:?}");
        assert!(harness.notes.join("2026-10-23.md").exists());
    }

    #[test]
    fn missing_notes_dir_aborts() {
        let harness = Harness::new("missing-dir");
        fs::remove_dir_all(&harness.notes).unwrap();
        let summary = harness.run();
        assert!(summary.aborted);
        assert!(summary
            .errors
            .iter()
            .any(|error| error.contains("笔记目录")));
    }

    #[test]
    fn conflict_copy_is_created_when_merge_is_impossible() {
        let backend = FakeBackend::strong();
        let first = Harness::with_backend("conflict-a", backend.clone());
        let second = Harness::with_backend("conflict-b", backend.clone());
        first.write("2026-10-23.md", "# 2026-10-23\n\n- 10:00 base\n");
        first.run();
        second.run();

        // 本地出现无法解析的块 → 放弃逐条合并。
        second.write("2026-10-23.md", "# 2026-10-23\n\n- 10:00\n");
        first.write(
            "2026-10-23.md",
            "# 2026-10-23\n\n- 10:00 base\n\n- 11:00 from-a\n",
        );
        first.run();

        let summary = second.run();
        assert_eq!(summary.conflicts, 1, "{summary:?}");

        let conflicts: Vec<_> = fs::read_dir(&second.notes)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.contains(".conflict-"))
            .collect();
        assert_eq!(conflicts.len(), 1, "{conflicts:?}");
        assert!(conflicts[0].starts_with("2026-10-23.conflict-test-pc-"));
    }

    #[test]
    fn conflict_names_match_protocol_rules() {
        let name = conflict_name("2026-10-23.md", "test-pc").unwrap();
        assert!(name.starts_with("2026-10-23.conflict-test-pc-"));
        assert!(name.ends_with(".md"));
        let suffix = name
            .strip_prefix("2026-10-23.conflict-")
            .unwrap()
            .strip_suffix(".md")
            .unwrap();
        assert!(suffix.len() <= 48);
        assert!(suffix
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-'));
    }

    #[test]
    fn device_slug_is_safe() {
        assert_eq!(device_slug("DESKTOP-ABC"), "desktop-abc");
        assert_eq!(device_slug("  笔记本  "), "device");
        assert_eq!(device_slug("a/b\\c"), "a-b-c");
    }

    #[test]
    fn note_names_reject_traversal_and_bad_suffixes() {
        assert!(is_valid_note_name("2026-10-23.md"));
        assert!(is_valid_note_name(
            "2026-10-23.conflict-pc-20261023101010.md"
        ));

        for bad in [
            "2026-10-23.conflict-x/../../evil.md",
            "2026-10-23.conflict-x\\..\\..\\evil.md",
            "2026-10-23.conflict-%2e%2e%2fevil.md",
            ".2026-10-23.md",
            "2026-10-23.conflict-UPPER.md",
            "2026-10-23.conflict-.md",
            "2026-10-23.md.md",
            "2026-10-23.conflict-x.md.bak",
            "2026-13-40.md",
        ] {
            assert!(!is_valid_note_name(bad), "{bad}");
        }
    }

    #[test]
    fn unsafe_remote_names_are_ignored() {
        let harness = Harness::new("unsafe-name");
        harness
            .backend
            .put_remote("2026-10-23.conflict-x/../../evil.md", b"evil".to_vec());

        let summary = harness.run();
        assert!(summary
            .warnings
            .iter()
            .any(|warning| warning.contains("文件名不合法")));
        assert!(!harness.root.join("evil.md").exists());
    }

    #[test]
    fn scan_marks_non_file_entries_as_skipped() {
        let harness = Harness::new("scan-nonfile");
        fs::create_dir_all(harness.notes.join("2026-10-23.md")).unwrap();

        let scan = scan_local(&harness.notes).unwrap();
        assert!(scan.skipped.contains("2026-10-23.md"));
        assert!(!scan.files.contains_key("2026-10-23.md"));
    }

    #[test]
    fn write_local_refuses_stale_snapshot() {
        let harness = Harness::new("stale-write");
        harness.write("2026-10-23.md", "old\n");
        let stale = sha256_base64(b"old\n");
        let engine = harness.engine();
        harness.write("2026-10-23.md", "new\n");

        let error = tauri::async_runtime::block_on(engine.write_local(
            "2026-10-23.md",
            b"remote\n".to_vec(),
            LocalExpect::Sha(stale),
        ))
        .unwrap_err();
        assert!(error.to_string().contains("同步期间被修改"), "{error}");
        assert_eq!(harness.read("2026-10-23.md"), "new\n");
    }

    #[test]
    fn remove_local_refuses_stale_snapshot() {
        let harness = Harness::new("stale-remove");
        harness.write("2026-10-23.md", "old\n");
        let stale = sha256_base64(b"old\n");
        let engine = harness.engine();
        harness.write("2026-10-23.md", "new\n");

        let error =
            tauri::async_runtime::block_on(engine.remove_local("2026-10-23.md", Some(stale)))
                .unwrap_err();
        assert!(error.to_string().contains("同步期间被修改"), "{error}");
        assert!(harness.notes.join("2026-10-23.md").exists());
    }

    #[test]
    fn upload_revive_rechecks_remote_presence() {
        let harness = Harness::new("revive-recheck");
        harness.write("2026-10-23.md", "# 2026-10-23\n\n- 10:00 a\n");
        assert_eq!(harness.run().uploaded, 1);

        harness.backend.hide_from_list("2026-10-23.md");
        harness.write("2026-10-23.md", "# 2026-10-23\n\n- 10:00 a\n\n- 11:00 b\n");
        let summary = harness.run();

        assert_eq!(summary.uploaded, 0, "{summary:?}");
        assert!(summary
            .warnings
            .iter()
            .any(|warning| warning.contains("远端仍存在")));
        assert!(harness.read("2026-10-23.md").contains("- 11:00 b"));
        assert!(harness
            .backend
            .content("2026-10-23.md")
            .unwrap()
            .contains("- 10:00 a"));
    }

    #[test]
    fn unreliable_listing_still_uploads_local_changes() {
        let backend = FakeBackend::new(Capabilities {
            strong_etag: false,
            honors_if_match: false,
            honors_if_none_match: false,
            reliable_listing: false,
        });
        let harness = Harness::with_backend("blind-upload", backend.clone());
        harness.write("2026-10-23.md", "# 2026-10-23\n\n- 10:00 a\n");
        assert_eq!(harness.run().uploaded, 1);

        // 远端确实被删掉了；列表不可靠，但本地有修改，仍应尝试复活上传。
        harness.backend.remove("2026-10-23.md");
        harness.write("2026-10-23.md", "# 2026-10-23\n\n- 10:00 a\n\n- 11:00 b\n");
        let summary = harness.run();

        assert_eq!(summary.uploaded, 1, "{summary:?}");
        assert!(backend
            .content("2026-10-23.md")
            .unwrap()
            .contains("- 11:00 b"));
    }

    #[test]
    fn trash_local_rechecks_remote_presence() {
        let harness = Harness::new("trash-recheck");
        harness.write("2026-10-23.md", "# 2026-10-23\n\n- 10:00 a\n");
        assert_eq!(harness.run().uploaded, 1);

        // 列表被截断：文件还在远端，但 list() 看不到它。
        harness.backend.hide_from_list("2026-10-23.md");
        let summary = harness.run();

        assert_eq!(summary.deleted, 0, "{summary:?}");
        assert!(harness.notes.join("2026-10-23.md").exists());
        assert!(summary
            .warnings
            .iter()
            .any(|warning| warning.contains("远端仍存在")));
    }
}
