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

// 引擎在 PR4 接线前只有测试会调用它。
#![allow(dead_code)]

pub(crate) mod backend;
pub(crate) mod merge;
pub(crate) mod state;

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
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// 删除安全阀：超过这个数量、且占比超过阈值才中止。
const DELETE_MIN_FILES: usize = 5;
const DELETE_RATIO_PERCENT: usize = 30;

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
            (true, false) => Action::Upload,
            (true, true) => Action::Merge,
            (false, true) => Action::Download,
            (false, false) => Action::Skip,
        };
    }

    if !remote_present {
        // 列表不可靠时不能把「没列出来」当成删除。
        if !caps.reliable_listing {
            return Action::Skip;
        }
        return match local {
            LocalState::Absent => Action::Forget,
            LocalState::Changed => Action::UploadRevive,
            LocalState::Unchanged => {
                if caps.supports_deletion() {
                    Action::TrashLocal
                } else {
                    // 弱服务器只上传：把本地内容补回远端。
                    Action::Upload
                }
            }
        };
    }

    match (local, remote_changed) {
        (LocalState::Absent, false) => {
            if caps.supports_deletion() {
                Action::DeleteRemote
            } else {
                Action::Forget
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
    pub size: u64,
    pub mtime_ns: u64,
    pub sha256: String,
}

#[derive(Default)]
struct LocalScan {
    files: BTreeMap<String, LocalFile>,
    /// 读不了的文件：按未知处理，不参与删除判断。
    skipped: BTreeSet<String>,
    warnings: Vec<String>,
}

/// 只同步日期笔记与冲突副本；`.bak-*`、`.tmp-*`、其它 Markdown 不碰。
fn is_sync_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".md") else {
        return false;
    };
    let Some(date) = stem.get(..10) else {
        return false;
    };
    if chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_err() {
        return false;
    }
    stem.len() == 10 || stem[10..].starts_with(".conflict-")
}

fn scan_local(dir: &Path) -> Result<LocalScan, SyncError> {
    let entries = fs::read_dir(dir)
        .map_err(|error| SyncError::Aborted(format!("无法读取笔记目录：{error}")))?;
    let mut scan = LocalScan::default();

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            scan.warnings
                .push("跳过文件名不是 UTF-8 的文件".to_string());
            continue;
        };
        if !is_sync_name(name) {
            continue;
        }
        let name = name.to_string();

        let meta = match fs::metadata(&path) {
            Ok(meta) => meta,
            Err(error) => {
                scan.warnings
                    .push(format!("{name}：无法读取文件信息（{error}）"));
                scan.skipped.insert(name);
                continue;
            }
        };
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
                size: bytes.len() as u64,
                mtime_ns: mtime_ns(meta.modified().unwrap_or(UNIX_EPOCH)),
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
        let (state, state_warning) = store.load();
        Self {
            backend,
            store,
            notes_dir,
            device_slug: device_slug(device_name),
            state,
            state_warning,
            io,
        }
    }

    pub(crate) fn capabilities(&self) -> Capabilities {
        self.backend.capabilities()
    }

    /// 跑一轮同步。除「目录不可用」外不返回 Err：单个文件的失败记在汇总里，
    /// 下一轮会重试。
    pub(crate) async fn run(&mut self) -> SyncSummary {
        let mut summary = SyncSummary::new();
        if let Some(warning) = self.state_warning.take() {
            summary.warnings.push(warning);
        }

        if !self.notes_dir.is_dir() {
            summary.aborted = true;
            summary.error("笔记目录", "不存在或不可读取，本轮已中止");
            summary.finished_at = Some(now_iso());
            return summary;
        }

        let scan = match scan_local(&self.notes_dir) {
            Ok(scan) => scan,
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
        if deletions > DELETE_MIN_FILES
            && deletions * 100 > tracked.saturating_mul(DELETE_RATIO_PERCENT)
        {
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
        if let Err(error) = self.store.save(&self.state) {
            summary.error("同步状态", error);
        }
        summary.finished_at = Some(now_iso());
        summary
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
        match action {
            Action::Skip => summary.skipped += 1,
            Action::Upload | Action::UploadRevive => {
                let Some(local) = local else {
                    summary.skipped += 1;
                    return;
                };
                let expect = match (action, &record, remote) {
                    (Action::Upload, Some(record), Some(_)) => {
                        Expect::Version(record.remote_version.clone())
                    }
                    _ => Expect::Absent,
                };
                match self.backend.put(name, &local.bytes, expect).await {
                    Ok(version) => {
                        self.commit(name, &local.bytes, version, summary);
                        summary.uploaded += 1;
                    }
                    Err(error) => summary.error(name, error),
                }
            }
            Action::Download | Action::RestoreLocal => match self.backend.get(name).await {
                Ok(Some((bytes, version))) => {
                    if let Err(error) = self.write_local(name, &bytes) {
                        summary.error(name, error);
                        return;
                    }
                    self.commit(name, &bytes, version, summary);
                    summary.downloaded += 1;
                }
                Ok(None) => {
                    summary
                        .warnings
                        .push(format!("{name}：远端文件已消失，留待下一轮"));
                }
                Err(error) => summary.error(name, error),
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
                        summary.error(name, error);
                        return;
                    }
                };
                let Some(remote_text) = String::from_utf8(remote_bytes).ok() else {
                    summary.error(name, "远端文件不是 UTF-8，已跳过");
                    return;
                };
                let base = self
                    .store
                    .read_base(name)
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .unwrap_or_default();

                match merge::merge(&base, &local.text, &remote_text) {
                    Some(result) => {
                        if result.text != local.text
                            && self.write_local(name, result.text.as_bytes()).is_err()
                        {
                            summary.error(name, "写入合并结果失败");
                            return;
                        }
                        match self
                            .backend
                            .put(
                                name,
                                result.text.as_bytes(),
                                Expect::Version(remote_version),
                            )
                            .await
                        {
                            Ok(version) => {
                                self.commit(name, result.text.as_bytes(), version, summary);
                                summary.merged += 1;
                                summary.conflicts += result.conflicts;
                            }
                            Err(error) => summary.error(name, error),
                        }
                    }
                    None => {
                        self.conflict_copy(name, local, &remote_text, remote_version, summary)
                            .await
                    }
                }
            }
            Action::DeleteRemote => {
                let Some(record) = record else {
                    return;
                };
                match self
                    .backend
                    .delete(name, Expect::Version(record.remote_version))
                    .await
                {
                    Ok(()) => {
                        self.forget(name);
                        summary.deleted += 1;
                    }
                    Err(SyncError::NotFound) => {
                        self.forget(name);
                        summary.deleted += 1;
                    }
                    Err(error) => summary.error(name, error),
                }
            }
            Action::TrashLocal => {
                let Some(local) = local else {
                    return;
                };
                if trash_dir.is_none() {
                    match self.store.new_trash_dir() {
                        Ok(dir) => *trash_dir = Some(dir),
                        Err(error) => {
                            summary.error(name, format!("创建 trash 目录失败：{error}"));
                            return;
                        }
                    }
                }
                let dir = trash_dir.as_ref().expect("trash 目录已创建");
                if let Err(error) = self.store.trash(dir, name, &local.bytes) {
                    summary.error(name, format!("备份到 trash 失败：{error}"));
                    return;
                }
                if let Err(error) = self.remove_local(name) {
                    summary.error(name, error);
                    return;
                }
                self.forget(name);
                summary.deleted += 1;
            }
            Action::Forget => self.forget(name),
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
        if let Err(error) = self.write_local(&conflict, remote_text.as_bytes()) {
            summary.error(name, error);
            return;
        }
        match self
            .backend
            .put(name, &local.bytes, Expect::Version(remote_version))
            .await
        {
            Ok(version) => {
                self.commit(name, &local.bytes, version, summary);
                summary.conflicts += 1;
                summary
                    .warnings
                    .push(format!("{name}：无法逐条合并，远端内容已保存为 {conflict}"));
            }
            Err(error) => summary.error(name, error),
        }
    }

    fn commit(
        &mut self,
        name: &str,
        bytes: &[u8],
        version: RemoteVersion,
        summary: &mut SyncSummary,
    ) {
        let path = self.notes_dir.join(name);
        let (size, modified) = match fs::metadata(&path) {
            Ok(meta) => (meta.len(), mtime_ns(meta.modified().unwrap_or(UNIX_EPOCH))),
            Err(_) => (bytes.len() as u64, 0),
        };
        let sha = sha256_base64(bytes);
        if let Err(error) = self.store.write_base(name, bytes) {
            summary
                .warnings
                .push(format!("{name}：保存 base 副本失败（{error}）"));
        }
        self.state.files.insert(
            name.to_string(),
            FileRecord {
                local_mtime_ns: modified,
                local_size: size,
                local_sha256: sha.clone(),
                remote_version: version,
                remote_sha256: sha,
                synced_at: now_iso(),
            },
        );
        if let Err(error) = self.store.save(&self.state) {
            summary.error(name, format!("保存同步状态失败：{error}"));
        }
    }

    fn forget(&mut self, name: &str) {
        self.state.files.remove(name);
        self.store.remove_base(name);
        let _ = self.store.save(&self.state);
    }

    fn write_local(&self, name: &str, bytes: &[u8]) -> Result<(), SyncError> {
        let path = self.notes_dir.join(name);
        {
            let _guard = self
                .io
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            write_atomic(&path, bytes)
                .map_err(|error| SyncError::Io(format!("写入 {name} 失败：{error}")))?;
        }
        notes::invalidate_cache(&path);
        Ok(())
    }

    fn remove_local(&self, name: &str) -> Result<(), SyncError> {
        let path = self.notes_dir.join(name);
        {
            let _guard = self
                .io
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            fs::remove_file(&path)
                .map_err(|error| SyncError::Io(format!("删除 {name} 失败：{error}")))?;
        }
        notes::invalidate_cache(&path);
        Ok(())
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
    let stamp = Local::now().format("%Y%m%d%H%M%S");
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
    }

    impl FakeBackend {
        fn new(caps: Capabilities) -> Self {
            Self {
                inner: Arc::new(FakeInner {
                    caps,
                    files: Mutex::new(BTreeMap::new()),
                    next: AtomicU64::new(1),
                    fail_list: AtomicBool::new(false),
                }),
            }
        }

        fn strong() -> Self {
            Self::new(Capabilities {
                strong_etag: true,
                honors_if_match: true,
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

        fn set_fail_list(&self, value: bool) {
            self.inner.fail_list.store(value, Ordering::SeqCst);
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
            Ok(self
                .inner
                .files
                .lock()
                .unwrap()
                .iter()
                .map(|(name, (bytes, version))| RemoteMeta {
                    name: name.clone(),
                    version: version.clone(),
                    size: bytes.len() as u64,
                })
                .collect())
        }

        async fn get(&self, name: &str) -> Result<Option<(Vec<u8>, RemoteVersion)>, SyncError> {
            Ok(self.inner.files.lock().unwrap().get(name).cloned())
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
                    Expect::Version(version) => match files.get(name) {
                        Some((_, current)) if current == version => {}
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
                if let Expect::Version(version) = &expect {
                    match files.get(name) {
                        Some((_, current)) if current == version => {}
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
            reliable_listing: true,
        };
        // 远端被删 → 把本地补回去，而不是删本地。
        assert_eq!(
            decide(&weak, true, LocalState::Unchanged, false, false),
            Action::Upload
        );
        // 本地删除 → 只忘掉记录，不发 DELETE。
        assert_eq!(
            decide(&weak, true, LocalState::Absent, true, false),
            Action::Forget
        );
        // 列表不可靠 → 远端缺失一律不动。
        let blind = Capabilities {
            strong_etag: false,
            honors_if_match: false,
            reliable_listing: false,
        };
        assert_eq!(
            decide(&blind, true, LocalState::Unchanged, false, false),
            Action::Skip
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
}
