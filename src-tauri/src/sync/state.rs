//! 同步状态：`state.json`、base 副本与 trash。
//!
//! 状态目录按笔记目录的哈希隔离：换目录等于换一份状态，不会把旧记录的删除
//! 传播到新目录。写入一律「临时文件 + rename」，与应用设置的落盘方式一致。

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use chrono::{Local, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::backend::RemoteVersion;

pub(crate) const STATE_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FileRecord {
    pub local_mtime_ns: u64,
    pub local_size: u64,
    pub local_sha256: String,
    pub remote_version: RemoteVersion,
    pub remote_sha256: String,
    pub synced_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SyncState {
    pub version: u32,
    pub files: BTreeMap<String, FileRecord>,
    pub last_sync_at: Option<String>,
}

impl Default for SyncState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            files: BTreeMap::new(),
            last_sync_at: None,
        }
    }
}

#[derive(Clone)]
pub(crate) struct StateStore {
    root: PathBuf,
}

impl StateStore {
    /// 状态目录：`<app_config_dir>/sync/<笔记目录哈希前 16 位>/`。
    /// 只给不会访问远端的测试用；真实调用一律走 [`StateStore::with_remote`]。
    #[cfg(test)]
    pub(crate) fn new(config_dir: &Path, notes_dir: &Path) -> Self {
        Self::with_remote(config_dir, notes_dir, "")
    }

    /// 远端身份也参与哈希：换服务器或换账号等于换一份状态。
    ///
    /// 否则新服务器上「一个文件都没有」会被当成「远端全部被删」，把本地
    /// 笔记整批扔进 trash——这是任何情况下都不能发生的误判。
    pub(crate) fn with_remote(config_dir: &Path, notes_dir: &Path, remote: &str) -> Self {
        let label = format!("{}\n{}", notes_dir.to_string_lossy(), remote.trim());
        Self {
            root: config_dir.join("sync").join(key_for(&label)),
        }
    }

    /// 0.5.0 用「原始 URL」做身份；升级后身份算法变了，旧状态目录会被当成新目录。
    /// 新目录还不存在时，把旧目录整体改名过来，避免一次全量重合并。
    pub(crate) fn with_remote_migrating(
        config_dir: &Path,
        notes_dir: &Path,
        remote: &str,
        legacy_remote: &str,
    ) -> Self {
        let store = Self::with_remote(config_dir, notes_dir, remote);
        if !store.root.exists() && !legacy_remote.trim().is_empty() {
            let legacy = Self::with_remote(config_dir, notes_dir, legacy_remote);
            if legacy.root != store.root && legacy.root.exists() {
                let _ = fs::rename(&legacy.root, &store.root);
            }
        }
        store
    }

    #[cfg(test)]
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    fn state_path(&self) -> PathBuf {
        self.root.join("state.json")
    }

    /// 读取状态；内容损坏时备份原文件并从空状态开始（首轮只会上传/下载，不会删除）。
    ///
    /// 读不出来（共享冲突、权限、杀软锁）返回 `Err`：这种错误不能当成空状态继续，
    /// 否则本轮会把所有文件当「无记录」重新合并，并在轮末覆盖掉原状态。
    pub(crate) fn load(&self) -> io::Result<(SyncState, Option<String>)> {
        let path = self.state_path();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok((SyncState::default(), None))
            }
            Err(error) => return Err(error),
        };

        Ok(match serde_json::from_slice::<SyncState>(&bytes) {
            Ok(state) if state.version <= STATE_VERSION => (state, None),
            Ok(state) => (
                SyncState::default(),
                Some(format!(
                    "同步状态版本 {} 高于当前支持的 {STATE_VERSION}，已重新开始",
                    state.version
                )),
            ),
            Err(error) => {
                let backup = self.backup_corrupt();
                let detail = backup
                    .map(|path| format!("，已备份到 {}", path.display()))
                    .unwrap_or_default();
                (
                    SyncState::default(),
                    Some(format!("同步状态已损坏（{error}）{detail}")),
                )
            }
        })
    }

    fn backup_corrupt(&self) -> Option<PathBuf> {
        let path = self.state_path();
        let target = self
            .root
            .join(format!("state.json.corrupt-{}", stamp(Local::now())));
        fs::rename(&path, &target).ok().map(|_| target)
    }

    pub(crate) fn save(&self, state: &SyncState) -> io::Result<()> {
        let data = serde_json::to_vec_pretty(state).map_err(io::Error::other)?;
        write_atomic(&self.state_path(), &data)
    }

    pub(crate) fn base_path(&self, name: &str) -> PathBuf {
        self.root.join("base").join(name)
    }

    pub(crate) fn read_base(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
        match fs::read(self.base_path(name)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            // 读失败与「没有 base」是两件事：前者必须让本轮放弃合并，
            // 否则会把正常删除当成新增。
            Err(error) => Err(error),
        }
    }

    pub(crate) fn write_base(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        write_atomic(&self.base_path(name), bytes)
    }

    pub(crate) fn remove_base(&self, name: &str) {
        let _ = fs::remove_file(self.base_path(name));
    }

    /// 为这一轮同步创建一个 trash 目录，避免同一秒内多次删除互相覆盖。
    pub(crate) fn new_trash_dir(&self) -> io::Result<PathBuf> {
        let base = stamp(Local::now());
        let trash = self.root.join("trash");
        fs::create_dir_all(&trash)?;
        for attempt in 0..100 {
            let candidate = if attempt == 0 {
                trash.join(&base)
            } else {
                trash.join(format!("{base}-{attempt}"))
            };
            match fs::create_dir(&candidate) {
                Ok(()) => return Ok(candidate),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::other("无法创建 trash 目录"))
    }

    pub(crate) fn trash(&self, dir: &Path, name: &str, bytes: &[u8]) -> io::Result<PathBuf> {
        let path = dir.join(name);
        write_atomic(&path, bytes)?;
        Ok(path)
    }
}

/// 状态目录名：SHA-256 取前 16 个 Base64url 字符，可直接做目录名。
fn key_for(label: &str) -> String {
    let hash = sha256_base64(label.as_bytes());
    hash.chars().take(16).collect()
}

pub(crate) fn sha256_base64(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

pub(crate) fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub(crate) fn mtime_ns(modified: SystemTime) -> u64 {
    modified
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0)
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state.json");
    let tmp = path.with_file_name(format!(".{name}.tmp"));
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

fn stamp(now: chrono::DateTime<Local>) -> String {
    now.format("%Y%m%d-%H%M%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "flashnote-sync-state-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn state_round_trips() {
        let dir = temp_dir("roundtrip");
        let store = StateStore::new(&dir, Path::new("D:\\Notes"));
        let mut state = SyncState::default();
        state.files.insert(
            "2026-10-23.md".into(),
            FileRecord {
                local_mtime_ns: 1,
                local_size: 2,
                local_sha256: "abc".into(),
                remote_version: RemoteVersion::Etag("\"v1\"".into()),
                remote_sha256: "def".into(),
                synced_at: now_iso(),
            },
        );
        store.save(&state).unwrap();

        let (loaded, warning) = store.load().unwrap();
        assert!(warning.is_none());
        assert_eq!(loaded.files.len(), 1);
        assert_eq!(
            loaded.files["2026-10-23.md"].remote_version,
            RemoteVersion::Etag("\"v1\"".into())
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_state_is_backed_up() {
        let dir = temp_dir("corrupt");
        let store = StateStore::new(&dir, Path::new("D:\\Notes"));
        fs::create_dir_all(store.root()).unwrap();
        fs::write(store.state_path(), b"{ not json").unwrap();

        let (state, warning) = store.load().unwrap();
        assert!(state.files.is_empty());
        assert!(warning.unwrap().contains("损坏"));
        let backups: Vec<_> = fs::read_dir(store.root())
            .unwrap()
            .filter_map(|item| item.ok())
            .filter(|item| item.file_name().to_string_lossy().contains("corrupt-"))
            .collect();
        assert_eq!(backups.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn remote_identity_changes_state_dir() {
        let dir = temp_dir("remote-key");
        let notes = Path::new("D:\\Notes");
        let first = StateStore::with_remote(&dir, notes, "https://a.example.com/\nalice");
        let second = StateStore::with_remote(&dir, notes, "https://b.example.com/\nalice");
        assert_ne!(
            first.root(),
            second.root(),
            "换服务器不能复用旧状态，否则会把本地笔记误判为远端已删除"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_state_dir_is_reused_after_identity_normalization() {
        let dir = temp_dir("migrate");
        let notes = Path::new("D:\\Notes");
        let legacy_remote = "https://DAV.example.com/dav\nalice";
        let legacy = StateStore::with_remote(&dir, notes, legacy_remote);
        fs::create_dir_all(legacy.root()).unwrap();
        fs::write(legacy.root().join("state.json"), b"{}").unwrap();

        let migrated = StateStore::with_remote_migrating(
            &dir,
            notes,
            "https://dav.example.com/dav\nalice",
            legacy_remote,
        );
        assert!(
            migrated.root().join("state.json").exists(),
            "旧状态应被复用"
        );
        assert!(!legacy.root().exists(), "旧目录应已改名");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn base_copies_and_trash_are_isolated_per_dir() {
        let dir = temp_dir("base");
        let first = StateStore::new(&dir, Path::new("D:\\Notes"));
        let second = StateStore::new(&dir, Path::new("D:\\Other"));
        assert_ne!(first.root(), second.root());

        first.write_base("2026-10-23.md", b"hello").unwrap();
        assert_eq!(
            first.read_base("2026-10-23.md").unwrap().as_deref(),
            Some(&b"hello"[..])
        );
        assert!(second.read_base("2026-10-23.md").unwrap().is_none());

        let trash = first.new_trash_dir().unwrap();
        let path = first.trash(&trash, "2026-10-23.md", b"bye").unwrap();
        assert_eq!(fs::read(path).unwrap(), b"bye");
        let _ = fs::remove_dir_all(&dir);
    }
}
