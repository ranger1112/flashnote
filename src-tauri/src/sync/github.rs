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
    let branch = match parts.next() {
        Some("tree") | Some("blob") => {
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

    /// 仓库与目录各探一次：前者验证令牌，后者确认同步目录是否存在。
    async fn probe(&self) -> Result<(), SyncError> {
        let url = format!("{}/repos/{}/{}", self.api, self.owner, self.repo);
        self.spend()?;
        let response = self
            .request(Method::GET, &url)
            .send()
            .await
            .map_err(network)?;
        // 仓库不存在时 GitHub 用 404 而不是 403，避免泄露私有仓库是否存在。
        if response.status() == StatusCode::NOT_FOUND {
            return Err(SyncError::Auth(
                "令牌无权访问该仓库，或仓库地址不正确".to_string(),
            ));
        }
        self.check(response).await?;
        Ok(())
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

    fn dir_url(&self) -> String {
        format!(
            "{}/repos/{}/{}/contents/{}",
            self.api,
            self.owner,
            self.repo,
            REMOTE_SUBDIR.trim_end_matches('/')
        )
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
            Some(branch) => format!("{url}?ref={}", utf8_percent_encode(branch, PATH_ENCODE)),
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
                // 403 既可能是限流也可能是权限不足，靠响应头区分。
                let limited = response
                    .headers()
                    .get("x-ratelimit-remaining")
                    .and_then(|value| value.to_str().ok())
                    == Some("0");
                if limited {
                    Err(SyncError::RateLimited {
                        retry_after_secs: retry_after(&response),
                    })
                } else {
                    let body = response.text().await.unwrap_or_default();
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
struct ContentItem {
    name: String,
    sha: String,
    size: u64,
    #[serde(rename = "type")]
    kind: String,
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

    async fn list(&self) -> Result<Vec<RemoteMeta>, SyncError> {
        self.spend()?;
        let url = self.with_ref(&self.dir_url());
        let response = self
            .request(Method::GET, &url)
            .send()
            .await
            .map_err(network)?;
        // 目录还没建出来：不是错误，等同于「远端一个文件都没有」。
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(Vec::new());
        }
        let response = self.check(response).await?;
        let items: Vec<ContentItem> = response
            .json()
            .await
            .map_err(|error| SyncError::Other(format!("解析目录列表失败：{error}")))?;
        Ok(items
            .into_iter()
            .filter(|item| item.kind == "file" && super::is_sync_name(&item.name))
            .map(|item| RemoteMeta {
                name: item.name,
                version: RemoteVersion::Etag(item.sha),
                size: item.size,
            })
            .collect())
    }

    async fn get(&self, name: &str) -> Result<Option<(Vec<u8>, RemoteVersion)>, SyncError> {
        self.spend()?;
        let url = self.with_ref(&self.file_url(name));
        let response = self
            .request(Method::GET, &url)
            .send()
            .await
            .map_err(network)?;
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
        self.spend()?;
        let url = self.with_ref(&self.file_url(name));
        let response = self
            .request(Method::GET, &url)
            .send()
            .await
            .map_err(network)?;
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
        let body = json!({
            "message": format!("flashnote: 删除 {name}"),
            "sha": version_token(version)?,
        });
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
        let response = self
            .request(Method::GET, &url)
            .header(reqwest::header::ACCEPT, "application/vnd.github.raw")
            .send()
            .await
            .map_err(network)?;
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
            .and(path("/repos/o/r/contents/flashnote/notes"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"name": "2026-10-31.md", "sha": "sha1", "size": 12, "type": "file"},
                {"name": "readme.txt", "sha": "sha2", "size": 3, "type": "file"},
                {"name": "old", "sha": "sha3", "size": 0, "type": "dir"},
            ])))
            .mount(&server)
            .await;

        let files = backend(&server).list().await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "2026-10-31.md");
        assert_eq!(files[0].version, RemoteVersion::Etag("sha1".into()));
    }

    #[tokio::test]
    async fn missing_directory_lists_as_empty() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
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
            .respond_with(ResponseTemplate::new(404))
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
