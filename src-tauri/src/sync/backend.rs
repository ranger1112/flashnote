//! 同步后端抽象：引擎只依赖这里，WebDAV 与未来的闪记云各自实现。

use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// 远端文件的版本标识。
///
/// WebDAV 优先用强 ETag；服务器不给 ETag 时退化为内容 SHA-256；
/// 闪记云以后用单调递增的整数版本号。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub(crate) enum RemoteVersion {
    Etag(String),
    Hash(String),
    Number(u64),
}

/// 服务器能力。探测失败时全部按最保守的假值处理。
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Capabilities {
    pub strong_etag: bool,
    pub honors_if_match: bool,
    pub reliable_listing: bool,
}

impl Capabilities {
    /// 只有三项都成立时才传播删除，避免在不支持条件写入的服务器上误删。
    pub(crate) fn supports_deletion(&self) -> bool {
        self.strong_etag && self.honors_if_match && self.reliable_listing
    }
}

/// 远端文件列表里的一项。
#[derive(Clone, Debug)]
pub(crate) struct RemoteMeta {
    pub name: String,
    pub version: RemoteVersion,
    pub size: u64,
}

/// 写入时要求的前置条件。
#[derive(Clone, Debug)]
pub(crate) enum Expect {
    /// 不检查版本。
    Any,
    /// 文件必须不存在。
    Absent,
    /// 文件版本必须匹配（If-Match）。
    Version(RemoteVersion),
}

#[derive(Clone, Debug)]
pub(crate) enum SyncError {
    Network(String),
    Auth(String),
    RateLimited {
        retry_after_secs: Option<u64>,
    },
    /// 423：文件被别的客户端锁住。
    Locked,
    /// 507：空间不足。
    Quota,
    NotFound,
    /// 412：版本不匹配，下一轮会重新合并。
    VersionMismatch,
    /// 405 / 501：服务器不支持该操作。
    Unsupported(String),
    Io(String),
    /// 整轮中止（目录不可用、安全阀触发等）。
    Aborted(String),
    Other(String),
}

impl SyncError {
    pub(crate) fn is_retryable(&self) -> bool {
        matches!(
            self,
            SyncError::Network(_)
                | SyncError::RateLimited { .. }
                | SyncError::Locked
                | SyncError::Other(_)
        )
    }

    /// 第 `attempt` 次重试前的等待时间（从 1 开始）。
    pub(crate) fn backoff(&self, attempt: u32) -> std::time::Duration {
        if let SyncError::RateLimited {
            retry_after_secs: Some(secs),
        } = self
        {
            return std::time::Duration::from_secs(*secs);
        }
        let secs = match attempt {
            0 | 1 => 1,
            2 => 5,
            3 => 30,
            _ => 120,
        };
        std::time::Duration::from_secs(secs)
    }
}

impl fmt::Display for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SyncError::Network(message) => write!(f, "网络错误：{message}"),
            SyncError::Auth(message) => write!(f, "认证失败：{message}"),
            SyncError::RateLimited { .. } => write!(f, "请求过于频繁，稍后自动重试"),
            SyncError::Locked => write!(f, "文件被服务器锁定，稍后自动重试"),
            SyncError::Quota => write!(f, "远端空间不足"),
            SyncError::NotFound => write!(f, "远端文件不存在"),
            SyncError::VersionMismatch => write!(f, "远端文件已被修改"),
            SyncError::Unsupported(message) => write!(f, "服务器不支持：{message}"),
            SyncError::Io(message) => write!(f, "本地文件错误：{message}"),
            SyncError::Aborted(message) => write!(f, "同步已中止：{message}"),
            SyncError::Other(message) => write!(f, "同步失败：{message}"),
        }
    }
}

impl std::error::Error for SyncError {}

#[async_trait]
pub(crate) trait SyncBackend: Send + Sync {
    fn capabilities(&self) -> Capabilities;

    /// 列出远端全部笔记文件；失败时调用方不得做任何删除。
    async fn list(&self) -> Result<Vec<RemoteMeta>, SyncError>;

    /// 读取文件；不存在返回 `Ok(None)`。
    async fn get(&self, name: &str) -> Result<Option<(Vec<u8>, RemoteVersion)>, SyncError>;

    /// 写入文件，成功返回新的版本。
    async fn put(
        &self,
        name: &str,
        data: &[u8],
        expect: Expect,
    ) -> Result<RemoteVersion, SyncError>;

    /// 删除文件。
    async fn delete(&self, name: &str, expect: Expect) -> Result<(), SyncError>;
}
