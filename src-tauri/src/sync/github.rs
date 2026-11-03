//! GitHub 仓库后端：把 Contents API 当成一个普通文件目录用。
//!
//! 版本用 GitHub 的 blob sha。它本身就是内容哈希，可以当强 ETag；PUT/DELETE
//! 带上 sha 等价于 `If-Match`，创建时不带 sha 而文件已存在会返回 422，所以
//! 「必须不存在」也天然成立。这三条正好对上 [`Capabilities`] 的三项能力，
//! 因此可以正常传播删除。
//!
//! 每个文件一次 PUT/DELETE = 一个 commit，历史会比较碎；换取的是实现简单、
//! 失败重试天然幂等（sha 不匹配就自动降级为冲突处理）。

use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use base64::Engine;
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde_json::json;

use super::backend::{Capabilities, Expect, RemoteMeta, RemoteVersion, SyncBackend, SyncError};
use super::webdav::REMOTE_SUBDIR;

const API_ROOT: &str = "https://api.github.com";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// 与 WebDAV 后端保持一致的单文件上限。
const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;
const UNLIMITED: usize = usize::MAX;
/// 只读请求遇到网络抖动时的轮内重试次数；与 WebDAV 后端保持一致的口径。
const MAX_RETRIES: u32 = 2;

/// 文件名里只需要转义会破坏 URL 的字符；`.`、`-`、`_` 保持原样。
const PATH_ENCODE: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}');

/// 查询串里还要转义 `+`、`&`、`=`：它们都是合法的分支名字符，
/// 不转义会被服务器当成空格或额外的参数。
const QUERY_ENCODE: &AsciiSet = &PATH_ENCODE.add(b'+').add(b'&').add(b'=').add(b';');

fn base64_engine() -> base64::engine::general_purpose::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

/// GitHub 仓库配置。令牌绝不进日志。
pub(crate) struct GitHubConfig {
    pub owner: String,
    pub repo: String,
    /// 留空表示仓库的默认分支。
    pub branch: Option<String>,
    pub token: String,
}

impl fmt::Debug for GitHubConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GitHubConfig")
            .field("owner", &self.owner)
            .field("repo", &self.repo)
            .field("branch", &self.branch)
            .field("token", &"***")
            .finish()
    }
}

/// 从仓库地址解析出 owner / repo / 分支。
///
/// 接受 `https://github.com/owner/repo`、带 `.git` 后缀、带 `/tree/<branch>`
/// 的页面地址，也接受省略协议头的写法。
pub(crate) fn parse_repo(url: &str) -> Option<(String, String, Option<String>)> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    let mut parts = without_scheme.split('/').filter(|part| !part.is_empty());
    let host = parts.next()?;
    if !host.eq_ignore_ascii_case("github.com") && !host.eq_ignore_ascii_case("www.github.com") {
        return None;
    }
    let owner = parts.next()?.trim().to_string();
    let repo = parts.next()?.trim().trim_end_matches(".git").to_string();
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    if !owner.chars().all(is_name_char) || !repo.chars().all(is_name_char) {
        return None;
    }
    // `.` / `..` 会被 URL 规范化吃掉，拼出来的 API 路径指向别处
    if owner.trim_matches('.').is_empty() || repo.trim_matches('.').is_empty() {
        return None;
    }
    let branch = match parts.next() {
        // `/tree/<分支>` 是分支页面；`/blob/<分支>/<文件>` 是文件页面，
        // 后面的路径不是分支名，宁可忽略也不能拼出一个不存在的分支。
        Some("tree") => {
            let rest: Vec<&str> = parts.collect();
            (!rest.is_empty()).then(|| rest.join("/"))
        }
        _ => None,
    };
    Some((owner, repo, branch))
}

/// GitHub 的用户名/仓库名只允许这些字符；`/` 与 `..` 在第 4 步之前就被切掉了。
fn is_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.')
}

pub(crate) struct GitHubBackend {
    client: reqwest::Client,
    api: String,
    owner: String,
    repo: String,
    branch: Option<String>,
    token: String,
    caps: Capabilities,
    budget: AtomicUsize,
}

impl GitHubBackend {
    /// 建立连接并验证令牌、仓库与目录可访问。
    pub(crate) async fn connect(config: GitHubConfig) -> Result<Self, SyncError> {
        let backend = Self::build(config, API_ROOT)?;
        backend.probe().await?;
        Ok(backend)
    }

    fn build(config: GitHubConfig, api: &str) -> Result<Self, SyncError> {
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("FlashNote/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| SyncError::Other(format!("初始化 HTTP 客户端失败：{error}")))?;

        Ok(Self {
            client,
            api: api.trim_end_matches('/').to_string(),
            owner: config.owner,
            repo: config.repo,
            branch: config.branch.filter(|value| !value.trim().is_empty()),
            token: config.token,
            // sha 可以当强 ETag；带 sha 写/删等价于 If-Match；不带 sha 创建已存在文件
            // 会被 GitHub 拒绝。三项都成立，所以允许传播删除。
            caps: Capabilities {
                strong_etag: true,
                honors_if_match: true,
                honors_if_none_match: true,
                reliable_listing: true,
            },
            budget: AtomicUsize::new(UNLIMITED),
        })
    }

    /// 只给测试用：把 API 根指向 wiremock。
    #[cfg(test)]
    pub(crate) fn with_api(config: GitHubConfig, api: &str) -> Result<Self, SyncError> {
        Self::build(config, api)
    }

    /// 验证令牌、仓库，以及显式配置的分支是否存在。
    ///
    /// 分支必须先确认：如果分支被改名或删除，`contents` 的 404 会被当成
    /// 「远端一个文件都没有」，进而把本地笔记全部当成已删除。
    async fn probe(&self) -> Result<(), SyncError> {
        let url = format!("{}/repos/{}/{}", self.api, self.owner, self.repo);
        let response = self.send_retry(|| self.request(Method::GET, &url)).await?;
        // 仓库不存在时 GitHub 用 404 而不是 403，避免泄露私有仓库是否存在。
        if response.status() == StatusCode::NOT_FOUND {
            return Err(SyncError::Auth(
                "令牌无权访问该仓库，或仓库地址不正确".to_string(),
            ));
        }
        self.check(response).await?;

        let Some(branch) = &self.branch else {
            return Ok(());
        };
        let url = format!(
            "{}/repos/{}/{}/branches/{}",
            self.api,
            self.owner,
            self.repo,
            utf8_percent_encode(branch, PATH_ENCODE)
        );
        let response = self.send_retry(|| self.request(Method::GET, &url)).await?;
        if response.status() == StatusCode::NOT_FOUND {
            // 空仓库还没有任何分支，第一次写入会创建它，不能因此报错。
            // 用 /commits 判断而不是仓库大小：size 按 KB 取整，极小的非空仓库
            // 也可能报 0，那样就把校验放空了。
            if self.repo_is_empty().await? {
                return Ok(());
            }
            return Err(SyncError::Auth(format!(
                "远端没有分支 {branch}，请检查“分支”这一栏"
            )));
        }
        self.check(response).await?;
        Ok(())
    }

    /// 只读请求的轮内重试：网络抖动/连接失败时重试两次，超时与 4xx 不重试。
    ///
    /// 只包 GET：PUT/DELETE 在「其实已经成功、只是响应丢了」的情况下重试
    /// 会拿到 409，被引擎当成并发修改，白白生成一次冲突。
    async fn send_retry<F>(&self, build: F) -> Result<reqwest::Response, SyncError>
    where
        F: Fn() -> reqwest::RequestBuilder,
    {
        let mut attempt = 0;
        loop {
            self.spend()?;
            match build().send().await {
                Ok(response) => return Ok(response),
                Err(error)
                    if (error.is_connect() || error.is_timeout()) && attempt < MAX_RETRIES =>
                {
                    attempt += 1;
                    let delay = Duration::from_secs(attempt as u64);
                    tokio::time::sleep(delay).await;
                }
                Err(error) => return Err(network(error)),
            }
        }
    }

    /// GitHub 对空仓库的 `/commits` 返回 409，用它判断比看仓库大小可靠。
    async fn repo_is_empty(&self) -> Result<bool, SyncError> {
        let url = format!(
            "{}/repos/{}/{}/commits?per_page=1",
            self.api, self.owner, self.repo
        );
        let response = self.send_retry(|| self.request(Method::GET, &url)).await?;
        if response.status() == StatusCode::CONFLICT {
            return Ok(true);
        }
        self.check(response).await?;
        Ok(false)
    }

    fn spend(&self) -> Result<(), SyncError> {
        let mut remaining = self.budget.load(Ordering::Relaxed);
        loop {
            if remaining == 0 {
                return Err(SyncError::Budget);
            }
            match self.budget.compare_exchange_weak(
                remaining,
                remaining - 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(()),
                Err(actual) => remaining = actual,
            }
        }
    }

    fn request(&self, method: Method, url: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, url)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", self.token),
            )
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
    }

    fn file_url(&self, name: &str) -> String {
        format!(
            "{}/repos/{}/{}/contents/{}{}",
            self.api,
            self.owner,
            self.repo,
            REMOTE_SUBDIR,
            utf8_percent_encode(name, PATH_ENCODE)
        )
    }

    /// 把 `?ref=<分支>` 拼进 URL。分支名允许 `/`（如 `feature/x`），
    /// 所以按路径段转义规则处理。
    fn with_ref(&self, url: &str) -> String {
        match &self.branch {
            Some(branch) => format!("{url}?ref={}", utf8_percent_encode(branch, QUERY_ENCODE)),
            None => url.to_string(),
        }
    }

    /// 把非 2xx 统一翻译成同步错误。
    async fn check(&self, response: reqwest::Response) -> Result<reqwest::Response, SyncError> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        match status {
            StatusCode::UNAUTHORIZED => Err(SyncError::Auth("GitHub 令牌无效或已过期".to_string())),
            StatusCode::NOT_FOUND => Err(SyncError::NotFound),
            StatusCode::CONFLICT | StatusCode::PRECONDITION_FAILED => {
                Err(SyncError::VersionMismatch)
            }
            StatusCode::TOO_MANY_REQUESTS => Err(SyncError::RateLimited {
                retry_after_secs: retry_after(&response),
            }),
            StatusCode::FORBIDDEN => {
                // 403 既可能是限流也可能是权限不足。主限流看 remaining=0；
                // 次级限流（并发/单位时间请求过多）remaining 仍是正数，只会带
                // retry-after 或在正文里说明，漏判会让整轮对着被限流的服务器硬打。
                let remaining_zero = response
                    .headers()
                    .get("x-ratelimit-remaining")
                    .and_then(|value| value.to_str().ok())
                    == Some("0");
                let has_retry_after = response
                    .headers()
                    .contains_key(reqwest::header::RETRY_AFTER);
                let wait = retry_after(&response);
                let body = response.text().await.unwrap_or_default();
                let mentions_limit = body.to_ascii_lowercase().contains("rate limit");
                if remaining_zero || has_retry_after || mentions_limit {
                    Err(SyncError::RateLimited {
                        // 次级限流的窗口通常是 1 分钟
                        retry_after_secs: wait.or(Some(60)),
                    })
                } else {
                    Err(SyncError::Forbidden(short(&body)))
                }
            }
            StatusCode::UNPROCESSABLE_ENTITY => {
                let body = response.text().await.unwrap_or_default();
                // 带 sha 的写入在版本不匹配时是 409；422 一般意味着「没带 sha 但文件已存在」
                if body.contains("sha") {
                    Err(SyncError::VersionMismatch)
                } else {
                    Err(SyncError::Other(format!(
                        "GitHub 拒绝了写入：{}",
                        short(&body)
                    )))
                }
            }
            other => {
                let body = response.text().await.unwrap_or_default();
                Err(SyncError::Network(format!(
                    "GitHub 返回 {other}：{}",
                    short(&body)
                )))
            }
        }
    }
}

fn network(error: reqwest::Error) -> SyncError {
    SyncError::Network(error.to_string())
}

fn retry_after(response: &reqwest::Response) -> Option<u64> {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse().ok())
}

/// 错误信息里只保留一小段，避免把整个 HTML 塞进状态栏。
fn short(body: &str) -> String {
    let text = body.trim();
    let limited: String = text.chars().take(200).collect();
    if limited.is_empty() {
        "（无错误信息）".to_string()
    } else {
        limited
    }
}

fn version_token(version: &RemoteVersion) -> Result<String, SyncError> {
    match version {
        RemoteVersion::Etag(value) | RemoteVersion::Hash(value) => Ok(value.clone()),
        RemoteVersion::Number(value) => Ok(value.to_string()),
    }
}

#[derive(Deserialize)]
struct TreeResponse {
    tree: Vec<TreeEntry>,
    #[serde(default)]
    truncated: bool,
}

#[derive(Deserialize)]
struct TreeEntry {
    path: String,
    sha: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    size: Option<u64>,
}

#[derive(Deserialize)]
struct ContentFile {
    content: String,
    encoding: String,
    sha: String,
}

#[derive(Deserialize)]
struct WriteResponse {
    content: Option<WrittenFile>,
}

#[derive(Deserialize)]
struct WrittenFile {
    sha: String,
}

#[async_trait::async_trait]
impl SyncBackend for GitHubBackend {
    fn capabilities(&self) -> Capabilities {
        self.caps.clone()
    }

    fn reset_budget(&self, requests: usize) {
        self.budget.store(requests, Ordering::Relaxed);
    }

    /// 用 Git Trees API 列目录。
    ///
    /// 不用 Contents API 的原因有两个：它对单次列表有 1000 项上限（日笔记攒到
    /// 两年多就会撑爆，且没有分页），而且「目录不存在」和「分支不存在」都返回
    /// 404 —— 后者会被当成「远端为空」，进而把本地笔记全部判成已删除。
    /// Trees 一次返回整棵树（上限 10 万项），ref 解析失败时只会是 404/409。
    async fn list(&self) -> Result<Vec<RemoteMeta>, SyncError> {
        let ref_name = self.branch.as_deref().unwrap_or("HEAD");
        let url = format!(
            "{}/repos/{}/{}/git/trees/{}?recursive=1",
            self.api,
            self.owner,
            self.repo,
            utf8_percent_encode(ref_name, PATH_ENCODE)
        );
        let response = self.send_retry(|| self.request(Method::GET, &url)).await?;

        if matches!(
            response.status(),
            StatusCode::NOT_FOUND | StatusCode::CONFLICT
        ) {
            // 空仓库还没有任何提交，等同于「远端一个文件都没有」。
            if self.repo_is_empty().await? {
                return Ok(Vec::new());
            }
            return Err(SyncError::Other(
                "远端分支不存在或已被删除，请检查同步设置".to_string(),
            ));
        }

        let response = self.check(response).await?;
        let tree: TreeResponse = response
            .json()
            .await
            .map_err(|error| SyncError::Other(format!("解析目录列表失败：{error}")))?;
        // 截断的列表会被引擎当成「其余文件都删了」，必须整轮中止。
        if tree.truncated {
            return Err(SyncError::Other(
                "远端文件过多，GitHub 返回了截断的目录列表，本轮已中止".to_string(),
            ));
        }

        Ok(tree
            .tree
            .into_iter()
            .filter(|entry| entry.kind == "blob")
            .filter_map(|entry| {
                // 只要同步目录下第一层的文件；子目录与其它路径都不碰。
                let name = entry.path.strip_prefix(REMOTE_SUBDIR)?;
                if name.contains('/') || !super::is_sync_name(name) {
                    return None;
                }
                Some(RemoteMeta {
                    name: name.to_string(),
                    version: RemoteVersion::Etag(entry.sha),
                    size: entry.size.unwrap_or(0),
                })
            })
            .collect())
    }

    async fn get(&self, name: &str) -> Result<Option<(Vec<u8>, RemoteVersion)>, SyncError> {
        let url = self.with_ref(&self.file_url(name));
        let response = self.send_retry(|| self.request(Method::GET, &url)).await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let response = self.check(response).await?;
        let file: ContentFile = response
            .json()
            .await
            .map_err(|error| SyncError::Other(format!("解析文件内容失败：{error}")))?;
        let bytes = if file.encoding == "base64" {
            base64_engine()
                .decode(file.content.split_whitespace().collect::<String>())
                .map_err(|error| SyncError::Other(format!("解码远端文件失败：{error}")))?
        } else {
            // 超过 1 MiB 时 Contents API 不返回内容，只能按原始内容再拉一次。
            self.fetch_raw(name).await?
        };
        if bytes.len() > MAX_FILE_BYTES {
            return Err(SyncError::TooLarge);
        }
        Ok(Some((bytes, RemoteVersion::Etag(file.sha))))
    }

    async fn exists(&self, name: &str) -> Result<bool, SyncError> {
        let url = self.with_ref(&self.file_url(name));
        let response = self.send_retry(|| self.request(Method::GET, &url)).await?;
        match response.status() {
            StatusCode::NOT_FOUND => Ok(false),
            status if status.is_success() => Ok(true),
            _ => {
                self.check(response).await?;
                Ok(false)
            }
        }
    }

    async fn put(
        &self,
        name: &str,
        data: &[u8],
        expect: Expect,
    ) -> Result<RemoteVersion, SyncError> {
        if data.len() > MAX_FILE_BYTES {
            return Err(SyncError::TooLarge);
        }
        let mut body = json!({
            "message": format!("flashnote: {name}"),
            "content": base64_engine().encode(data),
        });
        if let Some(branch) = &self.branch {
            body["branch"] = json!(branch);
        }
        // Absent 时不带 sha：GitHub 在文件已存在时返回 422，等价于 If-None-Match。
        if let Expect::Version { version, .. } = &expect {
            body["sha"] = json!(version_token(version)?);
        }

        self.spend()?;
        let response = self
            .request(Method::PUT, &self.file_url(name))
            .json(&body)
            .send()
            .await
            .map_err(network)?;
        let response = self.check(response).await?;
        let written: WriteResponse = response
            .json()
            .await
            .map_err(|error| SyncError::Other(format!("解析写入结果失败：{error}")))?;
        let sha = written
            .content
            .map(|content| content.sha)
            .ok_or_else(|| SyncError::Other("写入响应缺少版本信息".to_string()))?;
        Ok(RemoteVersion::Etag(sha))
    }

    async fn delete(&self, name: &str, expect: Expect) -> Result<(), SyncError> {
        let Expect::Version { version, .. } = &expect else {
            return Err(SyncError::Other("GitHub 删除需要版本信息".to_string()));
        };
        let mut body = json!({
            "message": format!("flashnote: 删除 {name}"),
            "sha": version_token(version)?,
        });
        // 不带 branch 时 GitHub 会去默认分支找这个 sha，非默认分支上必然对不上。
        if let Some(branch) = &self.branch {
            body["branch"] = json!(branch);
        }
        self.spend()?;
        let response = self
            .request(Method::DELETE, &self.file_url(name))
            .json(&body)
            .send()
            .await
            .map_err(network)?;
        self.check(response).await?;
        Ok(())
    }
}

impl GitHubBackend {
    /// Contents API 对 >1 MiB 的文件只给 sha 不给内容，这里按 raw 再拉一次。
    async fn fetch_raw(&self, name: &str) -> Result<Vec<u8>, SyncError> {
        self.spend()?;
        let url = self.with_ref(&self.file_url(name));
        let mut accept = reqwest::header::HeaderMap::new();
        accept.insert(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static("application/vnd.github.raw"),
        );
        let response = self
            .send_retry(|| self.request(Method::GET, &url).headers(accept.clone()))
            .await?;
        let response = self.check(response).await?;
        let bytes = response.bytes().await.map_err(network)?;
        Ok(bytes.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_repository_urls() {
        assert_eq!(
            parse_repo("https://github.com/ranger1112/flashnote"),
            Some(("ranger1112".to_string(), "flashnote".to_string(), None))
        );
        assert_eq!(
            parse_repo("github.com/ranger1112/flashnote.git"),
            Some(("ranger1112".to_string(), "flashnote".to_string(), None))
        );
        assert_eq!(
            parse_repo("https://github.com/ranger1112/flashnote/tree/dev"),
            Some((
                "ranger1112".to_string(),
                "flashnote".to_string(),
                Some("dev".to_string())
            ))
        );
        assert_eq!(parse_repo("https://gitlab.com/a/b"), None);
        assert_eq!(parse_repo("https://github.com/only-owner"), None);
        assert_eq!(parse_repo(""), None);
    }

    #[test]
    fn rejects_path_like_names() {
        assert_eq!(parse_repo("https://github.com/a/..%2F.."), None);
        // 仓库页面的子路径（issues、pull 等）只是多余尾巴，按仓库本身解析
        assert_eq!(
            parse_repo("https://github.com/a/b/issues"),
            Some(("a".to_string(), "b".to_string(), None))
        );
    }

    #[test]
    fn capabilities_allow_deletion() {
        let backend = GitHubBackend::build(
            GitHubConfig {
                owner: "o".into(),
                repo: "r".into(),
                branch: None,
                token: "t".into(),
            },
            API_ROOT,
        )
        .unwrap();
        assert!(backend.capabilities().supports_deletion());
    }

    #[test]
    fn debug_output_masks_the_token() {
        let config = GitHubConfig {
            owner: "o".into(),
            repo: "r".into(),
            branch: None,
            token: "ghp_secret".into(),
        };
        let text = format!("{config:?}");
        assert!(!text.contains("ghp_secret"), "{text}");
    }

    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn backend(server: &MockServer) -> GitHubBackend {
        GitHubBackend::with_api(
            GitHubConfig {
                owner: "o".into(),
                repo: "r".into(),
                branch: Some("main".into()),
                token: "t".into(),
            },
            &server.uri(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn list_keeps_only_note_files() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/git/trees/main"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "truncated": false,
                "tree": [
                    {"path": "flashnote/notes/2026-10-31.md", "sha": "sha1", "type": "blob", "size": 12},
                    {"path": "flashnote/notes/readme.txt", "sha": "sha2", "type": "blob", "size": 3},
                    {"path": "flashnote/notes/sub/deep.md", "sha": "sha3", "type": "blob", "size": 4},
                    {"path": "flashnote", "sha": "sha4", "type": "tree"},
                ]
            })))
            .mount(&server)
            .await;

        let files = backend(&server).list().await.unwrap();
        assert_eq!(files.len(), 1, "{files:?}");
        assert_eq!(files[0].name, "2026-10-31.md");
        assert_eq!(files[0].version, RemoteVersion::Etag("sha1".into()));
        assert_eq!(files[0].size, 12);
    }

    #[tokio::test]
    async fn empty_tree_lists_as_empty() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/git/trees/main"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"truncated": false, "tree": []})),
            )
            .mount(&server)
            .await;
        assert!(backend(&server).list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn truncated_listing_aborts_instead_of_reporting_deletions() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/git/trees/main"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"truncated": true, "tree": []})),
            )
            .mount(&server)
            .await;

        let error = backend(&server).list().await.unwrap_err();
        assert!(format!("{error:?}").contains("截断"), "{error:?}");
    }

    #[tokio::test]
    async fn missing_branch_is_an_error_not_an_empty_remote() {
        let server = MockServer::start().await;
        // 分支不存在 → 404；仓库非空（/commits 正常）→ 不能当成「远端为空」
        Mock::given(method("GET"))
            .and(path("/repos/o/r/git/trees/main"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;

        assert!(backend(&server).list().await.is_err());
    }

    #[tokio::test]
    async fn empty_repository_lists_as_empty() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/git/trees/main"))
            .respond_with(ResponseTemplate::new(409))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/commits"))
            .respond_with(ResponseTemplate::new(409))
            .mount(&server)
            .await;

        assert!(backend(&server).list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn get_decodes_base64_and_returns_the_blob_sha() {
        let server = MockServer::start().await;
        let encoded = base64_engine().encode("- 10:00 你好\n");
        Mock::given(method("GET"))
            .and(path("/repos/o/r/contents/flashnote/notes/2026-10-31.md"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "content": encoded,
                "encoding": "base64",
                "sha": "sha1",
            })))
            .mount(&server)
            .await;

        let (bytes, version) = backend(&server)
            .get("2026-10-31.md")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(String::from_utf8(bytes).unwrap(), "- 10:00 你好\n");
        assert_eq!(version, RemoteVersion::Etag("sha1".into()));
    }

    #[tokio::test]
    async fn put_sends_the_expected_sha_and_returns_the_new_version() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/repos/o/r/contents/flashnote/notes/2026-10-31.md"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "content": {"sha": "sha2"}
            })))
            .mount(&server)
            .await;

        let version = backend(&server)
            .put(
                "2026-10-31.md",
                b"- 10:00 hello\n",
                Expect::Version {
                    version: RemoteVersion::Etag("sha1".into()),
                    sha256: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(version, RemoteVersion::Etag("sha2".into()));

        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["sha"], "sha1");
        assert_eq!(body["branch"], "main");
    }

    #[tokio::test]
    async fn creating_an_existing_file_is_a_version_mismatch() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(422).set_body_json(json!({
                "message": "Invalid request.\n\n\"sha\" wasn't supplied."
            })))
            .mount(&server)
            .await;

        let error = backend(&server)
            .put("2026-10-31.md", b"x", Expect::Absent)
            .await
            .unwrap_err();
        assert!(matches!(error, SyncError::VersionMismatch), "{error:?}");
    }

    #[tokio::test]
    async fn delete_sends_the_sha() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/repos/o/r/contents/flashnote/notes/2026-10-31.md"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"commit": {}})))
            .mount(&server)
            .await;

        backend(&server)
            .delete(
                "2026-10-31.md",
                Expect::Version {
                    version: RemoteVersion::Etag("sha1".into()),
                    sha256: None,
                },
            )
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["sha"], "sha1");
        // 不带 branch 时 GitHub 会去默认分支找这个 sha，非默认分支上必然 409
        assert_eq!(body["branch"], "main");
    }

    #[tokio::test]
    async fn exhausted_rate_limit_is_reported_as_retryable() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(403)
                    .append_header("x-ratelimit-remaining", "0")
                    .append_header("retry-after", "60"),
            )
            .mount(&server)
            .await;

        let error = backend(&server).list().await.unwrap_err();
        assert!(
            matches!(
                error,
                SyncError::RateLimited {
                    retry_after_secs: Some(60)
                }
            ),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn budget_stops_further_requests() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"truncated": false, "tree": []})),
            )
            .mount(&server)
            .await;

        let backend = backend(&server);
        backend.reset_budget(1);
        assert!(backend.list().await.is_ok());
        assert!(matches!(
            backend.list().await.unwrap_err(),
            SyncError::Budget
        ));
    }
}
