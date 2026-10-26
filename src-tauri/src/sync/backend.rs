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
    /// `If-None-Match: *` 是否真的生效。它与 `If-Match` 是两种能力，
    /// 有的服务器只认其中一个，混用会让并发创建变成静默覆盖。
    pub honors_if_none_match: bool,
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
    /// 文件必须不存在。
    Absent,
    /// 远端内容必须仍是这个版本。
    ///
    /// `sha256` 是上一轮同步时看到的明文摘要：服务器不给 ETag、或 ETag 无法用于
    /// `If-Match` 时，后端先 GET 一次并比对摘要，避免退化成无条件覆盖。
    Version {
        version: RemoteVersion,
        sha256: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub(crate) enum SyncError {
    Network(String),
    /// 401：凭据无效/过期，整轮中止。
    Auth(String),
    /// 403：权限不足。通常只针对单个文件（只读、ACL），不应中止整轮。
    Forbidden(String),
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
    /// 本轮请求预算用尽；剩余文件留到下一轮，不算同步失败。
    Budget,
    /// 远端文件超过单文件上限；跳过即可，不算同步失败。
    TooLarge,
    /// 405 / 501：服务器不支持该操作。
    Unsupported(String),
    Io(String),
    /// 整轮中止（目录不可用、安全阀触发等）。
    Aborted(String),
    Other(String),
}

impl SyncError {
    /// 只有网络抖动、限流与文件锁值得自动重试；配置错误重试多少次都一样。
    pub(crate) fn is_retryable(&self) -> bool {
        matches!(
            self,
            SyncError::Network(_) | SyncError::RateLimited { .. } | SyncError::Locked
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
            SyncError::Forbidden(message) => write!(f, "没有权限：{message}"),
            SyncError::RateLimited { .. } => write!(f, "请求过于频繁，稍后自动重试"),
            SyncError::Locked => write!(f, "文件被服务器锁定，稍后自动重试"),
            SyncError::Quota => write!(f, "远端空间不足"),
            SyncError::NotFound => write!(f, "远端文件不存在"),
            SyncError::VersionMismatch => write!(f, "远端文件已被修改"),
            SyncError::Budget => write!(f, "本轮请求数已达上限，剩余文件留待下一轮"),
            SyncError::TooLarge => write!(f, "远端文件超过 2 MiB，已跳过"),
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

    /// 远端是否仍存在该文件；删除本地副本前用它复核列表。
    ///
    /// 列表可能被服务器或代理截断，而「没列出来」不能直接当成「已删除」。
    async fn exists(&self, name: &str) -> Result<bool, SyncError>;

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
