//! WebDAV 后端：PROPFIND/GET/PUT/DELETE、Basic Auth、重定向与能力探测。
//!
//! 服务器之间的差异很大：有的给强 ETag 并支持 `If-Match`，有的只有弱 ETag，
//! 有的干脆忽略条件请求。这里把差异收敛成 [`Capabilities`]：探测失败的项一律
//! 按「不支持」处理，引擎据此降级为只上传/下载，绝不冒误删的风险。
//!
//! 版本的取法：
//! - 有强 ETag 时用 ETag；
//! - 否则用 `Last-Modified + 长度` 组成的修订令牌。它不如内容哈希精确，但在
//!   没有 ETag 的服务器上已经是能用、且不会因为读不到内容而误判的方案。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use percent_encoding::percent_decode_str;
use quick_xml::events::Event;
use quick_xml::Reader;
use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, ETAG,
    IF_MATCH, IF_NONE_MATCH, LAST_MODIFIED, LOCATION, RETRY_AFTER,
};
use reqwest::{Client, Method, Response, StatusCode, Url};

use super::backend::{Capabilities, Expect, RemoteMeta, RemoteVersion, SyncBackend, SyncError};
use super::state::sha256_base64;
use super::MAX_FILE_BYTES;

/// 客户端只写自己的子目录，绝不碰用户放进去的其它文件。
pub(crate) const REMOTE_SUBDIR: &str = "flashnote/notes/";
const PROBE_NAME: &str = ".flashnote-probe";
const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?><d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/><d:getcontentlength/><d:getetag/><d:getlastmodified/></d:prop></d:propfind>"#;
const MAX_REDIRECTS: usize = 3;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(60);
/// 请求预算的「不限」哨兵，避免用 0 兼作「已用完」。
const UNLIMITED: usize = usize::MAX;
/// 单次请求内最多补两次；再失败就留给下一轮同步。
const MAX_RETRIES: u32 = 2;
/// `Retry-After` 可能长达一小时，单轮最多等这么久，剩下的下次再说。
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);
const DEPTH_HEADER: HeaderName = HeaderName::from_static("depth");

#[derive(Clone)]
pub(crate) struct WebDavConfig {
    /// 用户填写的根地址，例如 `https://dav.jianguoyun.com/dav/`。
    pub url: String,
    pub username: String,
    pub password: String,
    /// 允许非回环地址使用明文 `http://`。
    pub allow_insecure_http: bool,
}

impl std::fmt::Debug for WebDavConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 密码绝不进日志：手写 Debug，避免以后有人 `{:?}` 整个结构体。
        f.debug_struct("WebDavConfig")
            .field("url", &self.url)
            .field("username", &self.username)
            .field("password", &"***")
            .field("allow_insecure_http", &self.allow_insecure_http)
            .finish()
    }
}

pub(crate) struct WebDavBackend {
    client: Client,
    root: Url,
    base: Url,
    auth: String,
    caps: Capabilities,
    budget: AtomicUsize,
}

impl WebDavBackend {
    /// 建立连接并探测服务器能力。认证或网络错误会直接返回，便于「测试连接」使用。
    pub(crate) async fn connect(config: WebDavConfig) -> Result<Self, SyncError> {
        let mut backend = Self::build(config)?;
        backend.probe().await?;
        Ok(backend)
    }

    fn build(config: WebDavConfig) -> Result<Self, SyncError> {
        let root = parse_root(&config.url, config.allow_insecure_http)?;
        let base = root
            .join(REMOTE_SUBDIR)
            .map_err(|error| SyncError::Other(format!("地址无法拼接同步目录：{error}")))?;
        let auth = basic_auth(&config.username, &config.password);
        let client = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            // 重定向自己处理：跨源时必须丢掉 Authorization。
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("FlashNote/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| SyncError::Other(format!("初始化 HTTP 客户端失败：{error}")))?;

        Ok(Self {
            client,
            root,
            base,
            auth,
            caps: Capabilities::default(),
            budget: AtomicUsize::new(UNLIMITED),
        })
    }

    /// 下一轮允许发出的请求数；用尽后本轮剩余文件留到下一轮。
    pub(crate) fn reset_budget(&self, requests: usize) {
        self.budget.store(requests, Ordering::Relaxed);
    }

    fn take_budget(&self) -> Result<(), SyncError> {
        let previous = self
            .budget
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                if current == UNLIMITED || current == 0 {
                    Some(current)
                } else {
                    Some(current - 1)
                }
            })
            .unwrap_or(UNLIMITED);
        if previous == 0 {
            return Err(SyncError::Budget);
        }
        Ok(())
    }

    fn file_url(&self, name: &str) -> Result<Url, SyncError> {
        self.base
            .join(name)
            .map_err(|error| SyncError::Other(format!("文件名无法拼接：{error}")))
    }

    async fn probe(&mut self) -> Result<(), SyncError> {
        self.ensure_collections().await?;

        let probe_url = self.file_url(PROBE_NAME)?;
        let body = b"flashnote sync probe".to_vec();
        let plain = HeaderValue::from_static("text/plain; charset=utf-8");

        let (response, _) = self
            .request(
                Method::PUT,
                probe_url.clone(),
                vec![(CONTENT_TYPE, plain.clone())],
                Some(body.clone()),
                TRANSFER_TIMEOUT,
            )
            .await?;
        if !response.status().is_success() {
            return Err(map_status("写入测试文件", &response));
        }
        let strong_etag = response
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .map(is_strong_etag)
            .unwrap_or(false);

        // 用一个不可能匹配的 ETag 试写：返回 412 才说明条件写入真的生效。
        let bogus = HeaderValue::from_static("\"flashnote-probe-mismatch\"");
        let (response, _) = self
            .request(
                Method::PUT,
                probe_url.clone(),
                vec![(IF_MATCH, bogus)],
                Some(body.clone()),
                TRANSFER_TIMEOUT,
            )
            .await?;
        let honors_if_match = response.status() == StatusCode::PRECONDITION_FAILED;

        // `If-None-Match: *` 是另一种能力：有的服务器认 If-Match，却忽略它。
        // 探测文件此刻已经存在，条件写必须被拒。
        let (response, _) = self
            .request(
                Method::PUT,
                probe_url.clone(),
                vec![(IF_NONE_MATCH, HeaderValue::from_static("*"))],
                Some(body.clone()),
                TRANSFER_TIMEOUT,
            )
            .await?;
        let honors_if_none_match = response.status() == StatusCode::PRECONDITION_FAILED;

        let listed = match self.propfind(self.base.clone(), "1").await {
            Ok((xml, _)) => parse_multistatus(&xml)
                .map(|entries| {
                    entries.iter().any(|entry| {
                        !entry.is_missing()
                            && name_from_href(&entry.href).as_deref() == Some(PROBE_NAME)
                    })
                })
                .unwrap_or(false),
            Err(_) => false,
        };

        let readback_ok = matches!(
            self.read_bytes(probe_url.clone()).await,
            Ok((bytes, _)) if bytes == body
        );

        // 探测文件尽力清理，失败不影响能力判断。
        let _ = self
            .request(Method::DELETE, probe_url, Vec::new(), None, REQUEST_TIMEOUT)
            .await;

        self.caps = Capabilities {
            strong_etag,
            honors_if_match,
            honors_if_none_match,
            // 列表不完整或读回内容不一致时，按「列表不可靠」处理，绝不传播删除。
            reliable_listing: listed && readback_ok,
        };
        Ok(())
    }

    /// 远端目录不存在时补建。已存在（405/301）视为成功。
    async fn ensure_collections(&self) -> Result<(), SyncError> {
        for segment in ["flashnote/", REMOTE_SUBDIR] {
            let target = self
                .root
                .join(segment)
                .map_err(|error| SyncError::Other(format!("地址无法拼接：{error}")))?;
            let (response, _) = self
                .request(
                    dav_method(b"MKCOL"),
                    target,
                    Vec::new(),
                    None,
                    REQUEST_TIMEOUT,
                )
                .await?;
            let status = response.status();
            if status.is_success() {
                continue;
            }
            match status.as_u16() {
                // 已存在或父目录刚建好，都不是错误。
                301 | 405 | 409 => {}
                401 | 403 => return Err(map_status("创建同步目录", &response)),
                _ => return Err(SyncError::Other(format!("创建同步目录失败：HTTP {status}"))),
            }
        }
        Ok(())
    }

    async fn propfind(&self, url: Url, depth: &str) -> Result<(String, Url), SyncError> {
        let headers = vec![
            (
                DEPTH_HEADER,
                HeaderValue::from_str(depth).unwrap_or(HeaderValue::from_static("0")),
            ),
            (
                CONTENT_TYPE,
                HeaderValue::from_static("application/xml; charset=utf-8"),
            ),
        ];
        let (response, final_url) = self
            .request(
                dav_method(b"PROPFIND"),
                url,
                headers,
                Some(PROPFIND_BODY.as_bytes().to_vec()),
                REQUEST_TIMEOUT,
            )
            .await?;
        if !response.status().is_success() {
            return Err(map_status("读取远端目录", &response));
        }
        let text = response
            .text()
            .await
            .map_err(|error| SyncError::Network(format!("读取远端目录失败：{error}")))?;
        Ok((text, final_url))
    }

    async fn read_bytes(&self, url: Url) -> Result<(Vec<u8>, RemoteVersion), SyncError> {
        let (mut response, _) = self
            .request(Method::GET, url, Vec::new(), None, TRANSFER_TIMEOUT)
            .await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Err(SyncError::NotFound);
        }
        if !response.status().is_success() {
            return Err(map_status("下载", &response));
        }
        let version = response_version(response.headers());
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| SyncError::Network(format!("读取响应失败：{error}")))?
        {
            // 按块读取并在超限时立刻停下：Content-Length 缺失或被伪造时，
            // 也不会把整个超大响应读进内存。
            if bytes.len() as u64 + chunk.len() as u64 > MAX_FILE_BYTES {
                return Err(SyncError::TooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok((bytes, version))
    }

    /// Depth: 0 读单个文件的版本；服务器不支持时返回 None。
    async fn stat_version(&self, url: &Url) -> Option<RemoteVersion> {
        let (xml, _) = self.propfind(url.clone(), "0").await.ok()?;
        let entry = parse_multistatus(&xml)
            .ok()?
            .into_iter()
            .find(|entry| !entry.is_missing())?;
        Some(entry_version(&entry))
    }

    /// 没有可用的 `If-Match` 时，在写入/删除前复核远端内容。
    ///
    /// 服务器不给 ETag（或 ETag 无法用于条件请求）时，裸 PUT/DELETE 会静默覆盖
    /// 另一台设备刚写入的版本。这里先 GET 一次比对明文摘要；拿不到摘要才退回
    /// 版本令牌比对，两者都做不到就拒绝操作。
    async fn check_version(
        &self,
        url: &Url,
        version: &RemoteVersion,
        sha256: Option<&str>,
    ) -> Result<(), SyncError> {
        if let Some(expected) = sha256 {
            let (bytes, _) = self.read_bytes(url.clone()).await?;
            if sha256_base64(&bytes) != expected {
                return Err(SyncError::VersionMismatch);
            }
            return Ok(());
        }
        match self.stat_version(url).await {
            Some(current) if &current == version => Ok(()),
            Some(_) => Err(SyncError::VersionMismatch),
            None => Err(SyncError::Unsupported(
                "服务器不支持条件写入，无法安全覆盖".into(),
            )),
        }
    }

    /// 弱服务器写入后回读比对：内容被改写时重写一次，仍不一致就报错。
    async fn verify_write(&self, url: &Url, data: &[u8]) -> Result<(), SyncError> {
        for attempt in 0..2 {
            match self.read_bytes(url.clone()).await {
                Ok((bytes, _)) if bytes == data => return Ok(()),
                Ok(_) if attempt == 0 => {
                    self.request(
                        Method::PUT,
                        url.clone(),
                        vec![(
                            CONTENT_TYPE,
                            HeaderValue::from_static("text/markdown; charset=utf-8"),
                        )],
                        Some(data.to_vec()),
                        TRANSFER_TIMEOUT,
                    )
                    .await?;
                }
                Ok(_) => {}
                Err(SyncError::NotFound) => {}
                Err(error) => return Err(error),
            }
        }
        Err(SyncError::Unsupported(
            "写入后回读校验失败，服务器可能改写了内容".into(),
        ))
    }

    /// 发一个请求并手动跟随重定向；跨源时不再带 Authorization。
    /// 统一请求入口：网络抖动、429、423 自动退避重试，其它错误立刻返回。
    /// 所有方法（GET/PUT/DELETE/PROPFIND/MKCOL）都是幂等的，重试安全。
    async fn request(
        &self,
        method: Method,
        url: Url,
        headers: Vec<(HeaderName, HeaderValue)>,
        body: Option<Vec<u8>>,
        timeout: Duration,
    ) -> Result<(Response, Url), SyncError> {
        let mut attempt = 0;
        loop {
            match self
                .request_once(
                    method.clone(),
                    url.clone(),
                    headers.clone(),
                    body.clone(),
                    timeout,
                )
                .await
            {
                // 状态码错误由调用方映射；只有明确可重试的状态才在这里补发。
                Ok((response, final_url)) => {
                    let retryable = attempt < MAX_RETRIES
                        && matches!(response.status().as_u16(), 423 | 429 | 503);
                    if !retryable {
                        return Ok((response, final_url));
                    }
                    attempt += 1;
                    let delay = map_status("重试", &response)
                        .backoff(attempt)
                        .min(MAX_RETRY_DELAY);
                    log::warn!(
                        "WebDAV 响应 {}，{delay:?} 后第 {attempt} 次重试",
                        response.status()
                    );
                    tokio::time::sleep(delay).await;
                }
                Err(error) if error.is_retryable() && attempt < MAX_RETRIES => {
                    attempt += 1;
                    let delay = error.backoff(attempt).min(MAX_RETRY_DELAY);
                    log::warn!("WebDAV 请求失败（{error}），{delay:?} 后第 {attempt} 次重试");
                    tokio::time::sleep(delay).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn request_once(
        &self,
        method: Method,
        url: Url,
        headers: Vec<(HeaderName, HeaderValue)>,
        body: Option<Vec<u8>>,
        timeout: Duration,
    ) -> Result<(Response, Url), SyncError> {
        let origin = origin_of(&url);
        let secure_origin = url.scheme() == "https";
        let mut current = url;
        let mut method = method;
        let mut body = body;
        let mut with_auth = true;
        let mut hops = 0;

        loop {
            self.take_budget()?;
            let mut builder = self
                .client
                .request(method.clone(), current.clone())
                .timeout(timeout);
            if with_auth {
                builder = builder.header(AUTHORIZATION, self.auth.as_str());
            }
            for (name, value) in &headers {
                builder = builder.header(name, value);
            }
            if let Some(data) = body.as_ref() {
                builder = builder.body(data.clone());
            }
            let response = builder
                .send()
                .await
                .map_err(|error| SyncError::Network(format!("请求失败：{error}")))?;

            let status = response.status();
            if !is_redirect(status) {
                return Ok((response, current));
            }
            let Some(location) = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
            else {
                return Ok((response, current));
            };
            if hops >= MAX_REDIRECTS {
                return Err(SyncError::Network("重定向次数过多".into()));
            }
            // 303 的语义是「去另一个地址 GET 结果」，写请求跟随它等于把一次
            // 没有发生的写入当成成功。宁可让本轮失败，下一轮按新状态重新决策。
            if status == StatusCode::SEE_OTHER && method != Method::GET && method != Method::HEAD {
                return Err(SyncError::Other(
                    "服务器把写请求重定向为 303，已中止以免误判成功".into(),
                ));
            }
            let Ok(next) = current.join(location) else {
                return Ok((response, current));
            };
            let next_method = if status == StatusCode::SEE_OTHER {
                Method::GET
            } else {
                method.clone()
            };
            if origin_of(&next) != origin {
                // 跨源重定向只允许安全方法：否则笔记正文（PUT）或删除请求
                // 会被交给另一个主机。
                if next_method != Method::GET && next_method != Method::HEAD {
                    return Err(SyncError::Other(
                        "拒绝把带正文的请求重定向到其他主机".into(),
                    ));
                }
                // 恶意或配置错误的服务端可以把 GET 引到内网/回环地址，响应体还会
                // 被当成笔记内容写盘。跨源目标必须是公网 https。
                if next.scheme() != "https" || is_private_host(&next) {
                    return Err(SyncError::Other(
                        "拒绝把请求重定向到非公网 https 目标".into(),
                    ));
                }
                with_auth = false;
            } else if secure_origin && next.scheme() != "https" {
                // 同源也不允许 https → http 降级：正文与 Basic Auth 会明文出网。
                return Err(SyncError::Other("拒绝把 https 重定向降级为 http".into()));
            }
            if status == StatusCode::SEE_OTHER {
                method = next_method;
                body = None;
            }
            drop(response);
            current = next;
            hops += 1;
        }
    }
}

#[async_trait]
impl SyncBackend for WebDavBackend {
    fn capabilities(&self) -> Capabilities {
        self.caps.clone()
    }

    async fn list(&self) -> Result<Vec<RemoteMeta>, SyncError> {
        let (xml, final_url) = self.propfind(self.base.clone(), "1").await?;
        let entries = parse_multistatus(&xml)?;
        let collection = final_url.path().to_string();
        let mut metas = Vec::new();

        for entry in entries {
            // 集合自身、以及任何子目录都不是笔记；全是 404 propstat 的条目不存在。
            if entry.href.ends_with('/') || entry.href == collection || entry.is_missing() {
                continue;
            }
            let Some(name) = name_from_href(&entry.href) else {
                continue;
            };
            if !name.ends_with(".md") {
                continue;
            }
            metas.push(RemoteMeta {
                name,
                version: entry_version(&entry),
                size: entry.size.unwrap_or(0),
            });
        }
        Ok(metas)
    }

    async fn get(&self, name: &str) -> Result<Option<(Vec<u8>, RemoteVersion)>, SyncError> {
        let url = self.file_url(name)?;
        match self.read_bytes(url).await {
            Ok(value) => Ok(Some(value)),
            Err(SyncError::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn exists(&self, name: &str) -> Result<bool, SyncError> {
        let url = self.file_url(name)?;
        match self.propfind(url, "0").await {
            // 有的服务器用 `207` + 内嵌 404 propstat 表示资源不存在；解析器会把
            // 这类 response 过滤掉，所以看解析结果而不是只看 HTTP 状态。
            Ok((xml, _)) => match parse_multistatus(&xml) {
                // 只有明确看到「全是 404」才算不存在；解析成功但没有条目、或同一资源
                // 有一条非 404 的 response 时，保守当作存在，避免误覆盖远端。
                Ok(entries) => {
                    Ok(entries.is_empty() || entries.iter().any(|entry| !entry.is_missing()))
                }
                Err(_) => Ok(true),
            },
            Err(SyncError::NotFound) => Ok(false),
            Err(error) => Err(error),
        }
    }

    async fn put(
        &self,
        name: &str,
        data: &[u8],
        expect: Expect,
    ) -> Result<RemoteVersion, SyncError> {
        let url = self.file_url(name)?;
        let mut headers = vec![(
            CONTENT_TYPE,
            HeaderValue::from_static("text/markdown; charset=utf-8"),
        )];
        // 只有真的带上了条件头，服务器才可能替我们拒绝并发写入。
        let mut conditional = false;
        match &expect {
            Expect::Absent => {
                conditional = self.caps.honors_if_none_match;
                if conditional {
                    headers.push((IF_NONE_MATCH, HeaderValue::from_static("*")));
                } else {
                    // 没确认过 `If-None-Match: *` 是否生效：写前再确认一次远端确实
                    // 不存在，免得静默覆盖另一台设备刚创建的同名文件。头本身也不发，
                    // 有些服务器不是忽略而是直接拒绝这个头。
                    if self.exists(name).await? {
                        return Err(SyncError::VersionMismatch);
                    }
                }
            }
            Expect::Version {
                version: RemoteVersion::Etag(tag),
                ..
            } if self.caps.strong_etag && self.caps.honors_if_match => {
                headers.push((
                    IF_MATCH,
                    HeaderValue::from_str(tag)
                        .map_err(|error| SyncError::Other(format!("ETag 不合法：{error}")))?,
                ));
                conditional = true;
            }
            Expect::Version { version, sha256 } => {
                // 没有可用的 If-Match：先确认远端仍是预期内容，避免无条件覆盖。
                self.check_version(&url, version, sha256.as_deref()).await?;
            }
        }

        let mut response = self
            .request(
                Method::PUT,
                url.clone(),
                headers.clone(),
                Some(data.to_vec()),
                TRANSFER_TIMEOUT,
            )
            .await?
            .0;

        // 目录还没建好（首次同步、目录被外部删掉）时补建后重试一次。
        if matches!(
            response.status(),
            StatusCode::CONFLICT | StatusCode::NOT_FOUND
        ) {
            self.ensure_collections().await?;
            response = self
                .request(
                    Method::PUT,
                    url.clone(),
                    headers,
                    Some(data.to_vec()),
                    TRANSFER_TIMEOUT,
                )
                .await?
                .0;
        }

        if !response.status().is_success() {
            return Err(map_status("上传", &response));
        }

        let version = response
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .and_then(normalize_etag)
            .map(RemoteVersion::Etag);

        if !conditional {
            self.verify_write(&url, data).await?;
        }

        if let Some(version) = version {
            return Ok(version);
        }
        if let Some(version) = self.stat_version(&url).await {
            return Ok(version);
        }
        Ok(RemoteVersion::Hash(format!("-|{}", data.len())))
    }

    async fn delete(&self, name: &str, expect: Expect) -> Result<(), SyncError> {
        let url = self.file_url(name)?;
        let mut headers = Vec::new();
        let mut conditional = false;
        if let Expect::Version {
            version: RemoteVersion::Etag(tag),
            ..
        } = &expect
        {
            if self.caps.strong_etag && self.caps.honors_if_match {
                headers.push((
                    IF_MATCH,
                    HeaderValue::from_str(tag)
                        .map_err(|error| SyncError::Other(format!("ETag 不合法：{error}")))?,
                ));
                conditional = true;
            }
        }
        if !conditional {
            if let Expect::Version { version, sha256 } = &expect {
                self.check_version(&url, version, sha256.as_deref()).await?;
            }
        }
        let (response, _) = self
            .request(Method::DELETE, url, headers, None, REQUEST_TIMEOUT)
            .await?;
        if response.status().is_success() {
            return Ok(());
        }
        Err(map_status("删除", &response))
    }
}

// ---------------------------------------------------------------------------
// 纯函数：地址、状态码、XML
// ---------------------------------------------------------------------------

fn parse_root(raw: &str, allow_insecure_http: bool) -> Result<Url, SyncError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(SyncError::Other("请填写 WebDAV 地址".into()));
    }
    let mut url = Url::parse(trimmed)
        .map_err(|error| SyncError::Other(format!("地址格式不正确：{error}")))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(SyncError::Other(
            "地址里不要写账号密码，请分别填写在下面的输入框里".into(),
        ));
    }
    match url.scheme() {
        "https" => {}
        "http" => {
            if !allow_insecure_http && !is_loopback(&url) {
                return Err(SyncError::Other(
                    "http:// 会明文传输账号密码，请改用 https，或显式允许不安全连接".into(),
                ));
            }
        }
        other => return Err(SyncError::Other(format!("不支持的协议：{other}"))),
    }
    url.set_query(None);
    url.set_fragment(None);
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

/// 只校验地址格式，不联网；保存配置前调用。
pub(crate) fn validate_url(raw: &str, allow_insecure_http: bool) -> Result<(), SyncError> {
    parse_root(raw, allow_insecure_http).map(|_| ())
}

fn is_loopback(url: &Url) -> bool {
    match url.host_str() {
        Some(host) if host.eq_ignore_ascii_case("localhost") => true,
        // 必须解析成 IP：`127.evil.example` 这类域名不是回环地址。
        Some(host) => parse_ip(host).is_some_and(|ip| ip.is_loopback()),
        None => false,
    }
}

/// 解析 host（去掉 IPv6 的方括号）；域名返回 None。
fn parse_ip(host: &str) -> Option<std::net::IpAddr> {
    host.trim_matches(['[', ']']).parse().ok()
}

/// 跨源重定向的目标不允许指向回环、私网或链路本地地址（SSRF 防护）。
fn is_private_host(url: &Url) -> bool {
    match url.host_str() {
        Some(host) if host.eq_ignore_ascii_case("localhost") => true,
        Some(host) => parse_ip(host).is_some_and(|ip| match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
            }
            std::net::IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified(),
        }),
        None => true,
    }
}

fn basic_auth(username: &str, password: &str) -> String {
    // RFC 7617 名义上是 ISO-8859-1，但中文服务商实际都按 UTF-8 解码。
    let raw = format!("{username}:{password}");
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(raw.as_bytes())
    )
}

fn origin_of(url: &Url) -> (String, String, Option<u16>) {
    (
        url.scheme().to_string(),
        url.host_str().unwrap_or_default().to_ascii_lowercase(),
        url.port_or_known_default(),
    )
}

fn dav_method(name: &'static [u8]) -> Method {
    Method::from_bytes(name).expect("固定的 WebDAV 方法名总是合法")
}

fn is_redirect(status: StatusCode) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

fn map_status(context: &str, response: &Response) -> SyncError {
    let status = response.status();
    match status.as_u16() {
        401 | 403 => SyncError::Auth(format!("{context}：HTTP {}", status.as_u16())),
        404 => SyncError::NotFound,
        405 | 501 => SyncError::Unsupported(format!("{context}：HTTP {}", status.as_u16())),
        412 => SyncError::VersionMismatch,
        423 => SyncError::Locked,
        429 => SyncError::RateLimited {
            retry_after_secs: retry_after(response.headers()),
        },
        503 => match retry_after(response.headers()) {
            Some(secs) => SyncError::RateLimited {
                retry_after_secs: Some(secs),
            },
            None => SyncError::Network(format!("{context}：HTTP 503")),
        },
        507 => SyncError::Quota,
        _ => SyncError::Other(format!("{context}：HTTP {}", status.as_u16())),
    }
}

fn retry_after(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        // 超过一小时的等待没有意义，交给上层按轮次重试。
        .filter(|secs| *secs <= 3600)
}

/// 把服务器给的 ETag 规范成带引号的形式；弱 ETag 与空值返回 None。
///
/// 有的服务器 PUT 响应写 `"abc"`、PROPFIND 却写 `abc`（wsgidav 就是这样）。
/// 不统一的话，刚上传的文件下一轮会被误判成「远端已修改」而重复下载。
fn normalize_etag(etag: &str) -> Option<String> {
    let tag = etag.trim();
    if tag.is_empty() || tag.starts_with("W/") || tag.starts_with("w/") {
        return None;
    }
    let inner = tag.trim_matches('"');
    if inner.is_empty() || inner.contains('"') {
        return None;
    }
    Some(format!("\"{inner}\""))
}

fn is_strong_etag(etag: &str) -> bool {
    normalize_etag(etag).is_some()
}

fn version_token(modified: Option<&str>, size: u64) -> String {
    format!("{}|{size}", modified.unwrap_or("-"))
}

fn response_version(headers: &HeaderMap) -> RemoteVersion {
    if let Some(tag) = headers
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .and_then(normalize_etag)
    {
        return RemoteVersion::Etag(tag);
    }
    let modified = headers
        .get(LAST_MODIFIED)
        .and_then(|value| value.to_str().ok());
    let size = headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    RemoteVersion::Hash(version_token(modified, size))
}

fn entry_version(entry: &Props) -> RemoteVersion {
    if let Some(tag) = entry.etag.as_deref().and_then(normalize_etag) {
        return RemoteVersion::Etag(tag);
    }
    RemoteVersion::Hash(version_token(
        entry.modified.as_deref(),
        entry.size.unwrap_or(0),
    ))
}

/// 从 href 里取文件名；百分号编码按 UTF-8 解码。
fn name_from_href(href: &str) -> Option<String> {
    let path = href.split(['?', '#']).next().unwrap_or(href);
    let segment = path.rsplit('/').next()?;
    if segment.is_empty() {
        return None;
    }
    percent_decode_str(segment)
        .decode_utf8()
        .ok()
        .map(|name| name.into_owned())
}

#[derive(Default, Debug)]
pub(crate) struct Props {
    href: String,
    etag: Option<String>,
    size: Option<u64>,
    modified: Option<String>,
    /// 该 response 里是否出现过 2xx 的 propstat。
    ok: bool,
    /// 是否出现过 404 的 propstat。一个 response 常同时包含 200 与 404 两组
    /// （服务器不支持的属性走 404），只有「全是 404」才代表资源不存在。
    missing: bool,
}

impl Props {
    fn is_missing(&self) -> bool {
        self.missing && !self.ok
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Href,
    Etag,
    Size,
    Modified,
    Status,
}

/// `HTTP/1.1 404 Not Found` → `404`。
fn http_status_code(text: &str) -> Option<u16> {
    text.split_whitespace().nth(1)?.parse().ok()
}

/// 解析 207 Multistatus。命名空间前缀（`D:`、`d:`、`lp1:`）一律忽略。
///
/// 没有读到 `</multistatus>` 就当作列表被截断：宁可整轮中止，也不能凭半份
/// 列表去判断「远端文件被删了」。
fn parse_multistatus(xml: &str) -> Result<Vec<Props>, SyncError> {
    let malformed =
        |detail: String| SyncError::Unsupported(format!("远端目录列表无法解析：{detail}"));

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut entries: Vec<Props> = Vec::new();
    let mut current: Option<Props> = None;
    let mut field: Option<Field> = None;
    let mut text = String::new();
    let mut closed = false;

    loop {
        match reader.read_event() {
            Ok(Event::Eof) => break,
            Ok(Event::Start(event)) => match event.local_name().as_ref() {
                "response" => current = Some(Props::default()),
                "href" => {
                    field = Some(Field::Href);
                    text.clear();
                }
                "getetag" => {
                    field = Some(Field::Etag);
                    text.clear();
                }
                "getcontentlength" => {
                    field = Some(Field::Size);
                    text.clear();
                }
                "getlastmodified" => {
                    field = Some(Field::Modified);
                    text.clear();
                }
                "status" => {
                    field = Some(Field::Status);
                    text.clear();
                }
                _ => {}
            },
            Ok(Event::Text(event)) => {
                if field.is_some() {
                    text.push_str(&event.xml10_content());
                }
            }
            Ok(Event::CData(event)) => {
                if field.is_some() {
                    text.push_str(&event.into_inner());
                }
            }
            Ok(Event::End(event)) => {
                let local = event.local_name();
                match (local.as_ref(), field.take()) {
                    ("href", Some(Field::Href)) => {
                        if let Some(entry) = current.as_mut() {
                            entry.href = text.trim().to_string();
                        }
                    }
                    ("getetag", Some(Field::Etag)) => {
                        if let Some(entry) = current.as_mut() {
                            entry.etag = Some(text.trim().to_string());
                        }
                    }
                    ("getcontentlength", Some(Field::Size)) => {
                        if let Some(entry) = current.as_mut() {
                            entry.size = text.trim().parse::<u64>().ok();
                        }
                    }
                    ("getlastmodified", Some(Field::Modified)) => {
                        if let Some(entry) = current.as_mut() {
                            entry.modified = Some(text.trim().to_string());
                        }
                    }
                    ("status", Some(Field::Status)) => {
                        if let Some(entry) = current.as_mut() {
                            match http_status_code(&text) {
                                Some(code) if (200..300).contains(&code) => entry.ok = true,
                                Some(404) => entry.missing = true,
                                _ => {}
                            }
                        }
                    }
                    ("response", _) => {
                        if let Some(entry) = current.take() {
                            if !entry.href.is_empty() {
                                entries.push(entry);
                            }
                        }
                    }
                    ("multistatus", _) => closed = true,
                    _ => {}
                }
            }
            Ok(_) => {}
            Err(error) => return Err(malformed(error.to_string())),
        }
    }

    if !closed {
        return Err(malformed("响应不完整，可能被服务器截断".into()));
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn href_names_are_percent_decoded() {
        assert_eq!(
            name_from_href("/dav/flashnote/notes/2026-10-23.md").as_deref(),
            Some("2026-10-23.md")
        );
        assert_eq!(
            name_from_href("https://host/notes/2026-10-23.conflict-pc%201-20261023.md").as_deref(),
            Some("2026-10-23.conflict-pc 1-20261023.md")
        );
        assert_eq!(name_from_href("/dav/notes/"), None);
    }

    #[test]
    fn etags_are_normalized_across_quoted_and_bare_forms() {
        assert_eq!(normalize_etag("\"abc\"").as_deref(), Some("\"abc\""));
        assert_eq!(
            normalize_etag("abc-123-6").as_deref(),
            Some("\"abc-123-6\""),
            "不带引号的 ETag 也要和带引号的形式等价"
        );
        assert_eq!(normalize_etag("W/\"abc\""), None, "弱 ETag 不能当版本");
        assert_eq!(normalize_etag(""), None);
        assert_eq!(normalize_etag("\"a\"b\""), None);
    }

    #[test]
    fn plain_http_needs_explicit_permission() {
        assert!(parse_root("http://example.com/dav", false).is_err());
        assert!(parse_root("http://example.com/dav", true).is_ok());
        assert!(parse_root("http://127.0.0.1:5000", false).is_ok());
        assert!(parse_root("http://localhost:5000/", false).is_ok());
        // 前缀像回环的域名不是回环地址，明文仍然需要显式允许。
        assert!(parse_root("http://127.0.0.1.example.com/dav", false).is_err());
        assert!(parse_root("https://dav.example.com", false).is_ok());
    }

    #[test]
    fn root_url_gets_trailing_slash_and_sync_subdir() {
        let root = parse_root("https://dav.example.com/dav", false).unwrap();
        assert_eq!(root.as_str(), "https://dav.example.com/dav/");
        let base = root.join(REMOTE_SUBDIR).unwrap();
        assert_eq!(
            base.as_str(),
            "https://dav.example.com/dav/flashnote/notes/"
        );
    }

    #[test]
    fn credentials_with_userinfo_are_rejected() {
        assert!(parse_root("https://user:pw@dav.example.com/dav", false).is_err());
    }

    #[test]
    fn multistatus_without_closing_tag_is_truncated() {
        let xml = r#"<?xml version="1.0"?><D:multistatus xmlns:D="DAV:"><D:response><D:href>/a.md</D:href></D:response>"#;
        let error = parse_multistatus(xml).unwrap_err();
        assert!(matches!(error, SyncError::Unsupported(_)));
    }

    #[test]
    fn multistatus_ignores_namespace_prefixes() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>/dav/flashnote/notes/2026-10-23.md</D:href>
    <D:propstat><D:prop>
      <D:getcontentlength>42</D:getcontentlength>
      <D:getetag>"v1"</D:getetag>
      <D:getlastmodified>Fri, 23 Oct 2026 07:00:00 GMT</D:getlastmodified>
    </D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat>
  </D:response>
  <d:response xmlns:d="DAV:">
    <d:href>/dav/flashnote/notes/2026-10-23.conflict-pc%201-20261023.md</d:href>
    <d:propstat><d:prop>
      <d:getcontentlength>7</d:getcontentlength>
      <d:getetag>W/"weak"</d:getetag>
    </d:prop></d:propstat>
  </d:response>
</D:multistatus>"#;
        let entries = parse_multistatus(xml).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].size, Some(42));
        assert_eq!(entries[0].etag.as_deref(), Some("\"v1\""));
        assert_eq!(
            name_from_href(&entries[1].href).as_deref(),
            Some("2026-10-23.conflict-pc 1-20261023.md")
        );
        // 弱 ETag 不能当版本用，退化为修订令牌。
        assert!(matches!(entry_version(&entries[1]), RemoteVersion::Hash(_)));
    }

    // ---- 以下用 wiremock 覆盖真实 HTTP 行为 ----

    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const STRONG: Capabilities = Capabilities {
        strong_etag: true,
        honors_if_match: true,
        honors_if_none_match: true,
        reliable_listing: true,
    };

    fn config_for(server: &MockServer) -> WebDavConfig {
        WebDavConfig {
            url: server.uri(),
            username: "ranger".into(),
            password: "secret".into(),
            allow_insecure_http: false,
        }
    }

    fn backend_with(server: &MockServer, caps: Capabilities) -> WebDavBackend {
        let mut backend = WebDavBackend::build(config_for(server)).expect("构建后端");
        backend.caps = caps;
        backend
    }

    fn response_xml(href: &str, etag: Option<&str>, size: u64) -> String {
        let etag = etag
            .map(|tag| format!("<D:getetag>{tag}</D:getetag>"))
            .unwrap_or_default();
        format!(
            r#"<D:response><D:href>{href}</D:href><D:propstat><D:prop><D:resourcetype/>{etag}<D:getcontentlength>{size}</D:getcontentlength></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"#
        )
    }

    fn multistatus(items: &[String]) -> String {
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?><D:multistatus xmlns:D="DAV:">{}</D:multistatus>"#,
            items.concat()
        )
    }

    #[tokio::test]
    async fn list_returns_markdown_files_with_versions() {
        let server = MockServer::start().await;
        let body = multistatus(&[
            response_xml("/flashnote/notes/", None, 0),
            response_xml("/flashnote/notes/2026-10-23.md", Some("\"v1\""), 12),
            response_xml("/flashnote/notes/2026-10-22.md", None, 7),
            response_xml("/flashnote/notes/readme.txt", Some("\"x\""), 3),
        ]);
        Mock::given(method("PROPFIND"))
            .and(path("/flashnote/notes/"))
            .and(header("depth", "1"))
            .respond_with(ResponseTemplate::new(207).set_body_string(body))
            .mount(&server)
            .await;

        let mut metas = backend_with(&server, STRONG).list().await.unwrap();
        metas.sort_by(|left, right| left.name.cmp(&right.name));
        let names: Vec<_> = metas.iter().map(|meta| meta.name.as_str()).collect();
        assert_eq!(names, ["2026-10-22.md", "2026-10-23.md"]);
        assert_eq!(metas[1].version, RemoteVersion::Etag("\"v1\"".into()));
        assert_eq!(metas[1].size, 12);
        // 没有 ETag 的文件退化为修订令牌。
        assert!(matches!(metas[0].version, RemoteVersion::Hash(_)));
    }

    #[tokio::test]
    async fn put_sends_if_match_when_server_supports_it() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/flashnote/notes/2026-10-23.md"))
            .and(header("if-match", "\"v1\""))
            .respond_with(ResponseTemplate::new(204).insert_header("etag", "\"v2\""))
            .mount(&server)
            .await;

        let version = backend_with(&server, STRONG)
            .put(
                "2026-10-23.md",
                b"- 10:00 hi\n",
                Expect::Version {
                    version: RemoteVersion::Etag("\"v1\"".into()),
                    sha256: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(version, RemoteVersion::Etag("\"v2\"".into()));
    }

    #[tokio::test]
    async fn write_redirected_with_303_is_not_treated_as_success() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/flashnote/notes/2026-10-23.md"))
            .respond_with(
                ResponseTemplate::new(303)
                    .insert_header("location", "/flashnote/notes/2026-10-23.md"),
            )
            .mount(&server)
            .await;
        // 如果客户端跟随了 303，会命中这个 GET 并把它当成写成功。
        Mock::given(method("GET"))
            .and(path("/flashnote/notes/2026-10-23.md"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"old".to_vec()))
            .expect(0)
            .mount(&server)
            .await;

        let error = backend_with(&server, STRONG)
            .put("2026-10-23.md", b"new\n", Expect::Absent)
            .await
            .unwrap_err();
        assert!(matches!(error, SyncError::Other(_)), "{error:?}");
    }

    #[tokio::test]
    async fn get_follows_303_redirect() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/flashnote/notes/2026-10-23.md"))
            .respond_with(
                ResponseTemplate::new(303).insert_header("location", "/flashnote/notes/target.md"),
            )
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/flashnote/notes/target.md"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"hello".to_vec()))
            .mount(&server)
            .await;

        let (bytes, _) = backend_with(&server, STRONG)
            .get("2026-10-23.md")
            .await
            .unwrap()
            .expect("应有内容");
        assert_eq!(bytes, b"hello");
    }

    #[test]
    fn partial_404_propstat_keeps_the_entry() {
        // 弱服务器常把不支持的 getetag 放进 404 propstat，文件本身仍然存在。
        let xml = r#"<?xml version="1.0" encoding="utf-8"?><D:multistatus xmlns:D="DAV:"><D:response><D:href>/flashnote/notes/2026-10-23.md</D:href><D:propstat><D:prop><D:getcontentlength>12</D:getcontentlength></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat><D:propstat><D:prop><D:getetag/></D:prop><D:status>HTTP/1.1 404 Not Found</D:status></D:propstat></D:response></D:multistatus>"#;
        let entries = parse_multistatus(xml).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].is_missing());
        assert_eq!(entries[0].size, Some(12));
    }

    #[tokio::test]
    async fn exists_is_true_when_only_some_properties_are_404() {
        let server = MockServer::start().await;
        Mock::given(method("PROPFIND"))
            .and(path("/flashnote/notes/2026-10-23.md"))
            .respond_with(ResponseTemplate::new(207).set_body_string(
                r#"<?xml version="1.0" encoding="utf-8"?><D:multistatus xmlns:D="DAV:"><D:response><D:href>/flashnote/notes/2026-10-23.md</D:href><D:propstat><D:prop><D:getcontentlength>12</D:getcontentlength></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat><D:propstat><D:prop><D:getetag/></D:prop><D:status>HTTP/1.1 404 Not Found</D:status></D:propstat></D:response></D:multistatus>"#,
            ))
            .mount(&server)
            .await;

        assert!(backend_with(&server, STRONG)
            .exists("2026-10-23.md")
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn exists_is_false_for_207_with_embedded_404() {
        let server = MockServer::start().await;
        Mock::given(method("PROPFIND"))
            .and(path("/flashnote/notes/2026-10-23.md"))
            .respond_with(ResponseTemplate::new(207).set_body_string(
                r#"<?xml version="1.0" encoding="utf-8"?><D:multistatus xmlns:D="DAV:"><D:response><D:href>/flashnote/notes/2026-10-23.md</D:href><D:propstat><D:prop/><D:status>HTTP/1.1 404 Not Found</D:status></D:propstat></D:response></D:multistatus>"#,
            ))
            .mount(&server)
            .await;

        assert!(!backend_with(&server, STRONG)
            .exists("2026-10-23.md")
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn absent_write_checks_existence_when_if_none_match_is_not_honored() {
        let server = MockServer::start().await;
        Mock::given(method("PROPFIND"))
            .and(path("/flashnote/notes/2026-10-23.md"))
            .respond_with(
                ResponseTemplate::new(207).set_body_string(multistatus(&[response_xml(
                    "/flashnote/notes/2026-10-23.md",
                    Some("\"v1\""),
                    12,
                )])),
            )
            .mount(&server)
            .await;
        // 远端已经存在时绝不能无条件写入。
        Mock::given(method("PUT"))
            .and(path("/flashnote/notes/2026-10-23.md"))
            .respond_with(ResponseTemplate::new(204))
            .expect(0)
            .mount(&server)
            .await;

        let caps = Capabilities {
            strong_etag: true,
            honors_if_match: true,
            honors_if_none_match: false,
            reliable_listing: true,
        };
        let error = backend_with(&server, caps)
            .put("2026-10-23.md", b"new\n", Expect::Absent)
            .await
            .unwrap_err();
        assert!(matches!(error, SyncError::VersionMismatch), "{error:?}");
    }

    #[tokio::test]
    async fn hash_version_is_checked_before_overwrite() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/flashnote/notes/2026-10-23.md"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"newer".to_vec()))
            .mount(&server)
            .await;
        // 预检发现远端已变，PUT 不应发出。
        Mock::given(method("PUT"))
            .and(path("/flashnote/notes/2026-10-23.md"))
            .respond_with(ResponseTemplate::new(204))
            .expect(0)
            .mount(&server)
            .await;

        let error = backend_with(&server, STRONG)
            .put(
                "2026-10-23.md",
                b"- 10:00 hi\n",
                Expect::Version {
                    version: RemoteVersion::Hash("rev-1".into()),
                    sha256: Some(sha256_base64(b"- 10:00 base\n")),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error, SyncError::VersionMismatch), "{error}");
    }

    #[tokio::test]
    async fn hash_version_is_written_when_content_matches() {
        let server = MockServer::start().await;
        // 预检看到旧内容，回读看到刚写入的内容。
        Mock::given(method("GET"))
            .and(path("/flashnote/notes/2026-10-23.md"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"- 10:00 base\n".to_vec()))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/flashnote/notes/2026-10-23.md"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"- 10:00 hi\n".to_vec()))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/flashnote/notes/2026-10-23.md"))
            .respond_with(ResponseTemplate::new(204).insert_header("etag", "\"v2\""))
            .mount(&server)
            .await;

        let version = backend_with(&server, STRONG)
            .put(
                "2026-10-23.md",
                b"- 10:00 hi\n",
                Expect::Version {
                    version: RemoteVersion::Hash("rev-1".into()),
                    sha256: Some(sha256_base64(b"- 10:00 base\n")),
                },
            )
            .await
            .unwrap();
        assert_eq!(version, RemoteVersion::Etag("\"v2\"".into()));
    }

    #[tokio::test]
    async fn weak_server_writes_are_verified_by_readback() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/flashnote/notes/2026-10-23.md"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/flashnote/notes/2026-10-23.md"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"- 10:00 hi\n".to_vec()))
            .mount(&server)
            .await;

        let version = backend_with(&server, Capabilities::default())
            .put("2026-10-23.md", b"- 10:00 hi\n", Expect::Absent)
            .await
            .unwrap();
        assert!(matches!(version, RemoteVersion::Hash(_)));
    }

    #[tokio::test]
    async fn rewritten_content_fails_readback_verification() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/flashnote/notes/2026-10-23.md"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/flashnote/notes/2026-10-23.md"))
            .respond_with(
                ResponseTemplate::new(200).set_body_bytes(b"server rewrote this".to_vec()),
            )
            .mount(&server)
            .await;

        let error = backend_with(&server, Capabilities::default())
            .put("2026-10-23.md", b"- 10:00 hi\n", Expect::Absent)
            .await
            .unwrap_err();
        assert!(matches!(error, SyncError::Unsupported(_)));
    }

    #[tokio::test]
    async fn transient_errors_are_retried_once() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/flashnote/notes/2026-10-23.md"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/flashnote/notes/2026-10-23.md"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"- 10:00 hi\n".to_vec()))
            .mount(&server)
            .await;

        let (bytes, _) = backend_with(&server, STRONG)
            .get("2026-10-23.md")
            .await
            .unwrap()
            .expect("重试后应读到文件");
        assert_eq!(bytes, b"- 10:00 hi\n");

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2, "503 应被自动重试一次");
    }

    #[tokio::test]
    async fn http_errors_map_to_actionable_variants() {
        let server = MockServer::start().await;
        for (name, status) in [
            ("401.md", 401),
            ("404.md", 404),
            ("423.md", 423),
            ("507.md", 507),
        ] {
            Mock::given(method("GET"))
                .and(path(format!("/flashnote/notes/{name}")))
                .respond_with(ResponseTemplate::new(status))
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/flashnote/notes/429.md"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "7"))
            .mount(&server)
            .await;

        let backend = backend_with(&server, STRONG);
        assert!(matches!(
            backend.get("401.md").await,
            Err(SyncError::Auth(_))
        ));
        assert!(matches!(
            backend.get("423.md").await,
            Err(SyncError::Locked)
        ));
        assert!(matches!(backend.get("507.md").await, Err(SyncError::Quota)));
        match backend.get("429.md").await {
            Err(SyncError::RateLimited { retry_after_secs }) => {
                assert_eq!(retry_after_secs, Some(7));
            }
            other => panic!("期望限流错误，实际是 {other:?}"),
        }
        assert!(matches!(backend.get("404.md").await, Ok(None)));
    }

    #[tokio::test]
    async fn same_origin_redirects_keep_credentials() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/flashnote/notes/2026-10-23.md"))
            .respond_with(
                ResponseTemplate::new(301).insert_header("location", "/flashnote/notes/moved.md"),
            )
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/flashnote/notes/moved.md"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"hello".to_vec()))
            .mount(&server)
            .await;

        let (bytes, _) = backend_with(&server, STRONG)
            .get("2026-10-23.md")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(bytes, b"hello");

        let requests = server.received_requests().await.unwrap();
        assert!(requests
            .iter()
            .all(|request| request.headers.get("authorization").is_some()));
    }

    #[tokio::test]
    async fn cross_origin_redirect_to_private_host_is_rejected() {
        let target = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&target)
            .await;

        let origin = MockServer::start().await;
        let location = format!("{}/flashnote/notes/2026-10-23.md", target.uri());
        Mock::given(method("GET"))
            .and(path("/flashnote/notes/2026-10-23.md"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", location))
            .mount(&origin)
            .await;

        let error = backend_with(&origin, STRONG)
            .get("2026-10-23.md")
            .await
            .unwrap_err();
        assert!(matches!(error, SyncError::Other(_)), "{error}");
        assert!(target.received_requests().await.unwrap().is_empty());
    }

    #[test]
    fn loopback_detection_requires_a_real_ip() {
        let parse = |value: &str| Url::parse(value).unwrap();
        assert!(is_loopback(&parse("http://127.0.0.1:5000/")));
        assert!(is_loopback(&parse("http://[::1]:5000/")));
        assert!(is_loopback(&parse("http://localhost:5000/")));
        assert!(!is_loopback(&parse("http://127.0.0.1.example.com/")));
        assert!(!is_loopback(&parse("http://127.attacker.tld/")));
        assert!(is_private_host(&parse("https://10.0.0.5/")));
        assert!(is_private_host(&parse("https://[::1]/")));
        assert!(!is_private_host(&parse("https://dav.example.com/")));
    }

    #[test]
    fn debug_output_redacts_the_password() {
        let config = WebDavConfig {
            url: "https://dav.example.com/dav/".into(),
            username: "ranger".into(),
            password: "top-secret".into(),
            allow_insecure_http: false,
        };
        let text = format!("{config:?}");
        assert!(!text.contains("top-secret"), "{text}");
        assert!(text.contains("***"));
    }

    #[tokio::test]
    async fn cross_origin_redirect_does_not_receive_the_body() {
        let target = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&target)
            .await;

        let origin = MockServer::start().await;
        let location = format!("{}/flashnote/notes/2026-10-23.md", target.uri());
        Mock::given(method("PUT"))
            .and(path("/flashnote/notes/2026-10-23.md"))
            .respond_with(ResponseTemplate::new(301).insert_header("location", location))
            .mount(&origin)
            .await;

        let error = backend_with(&origin, STRONG)
            .put("2026-10-23.md", b"- 10:00 hi\n", Expect::Absent)
            .await
            .unwrap_err();
        assert!(matches!(error, SyncError::Other(_)), "{error}");
    }

    #[tokio::test]
    async fn exhausted_budget_is_reported_as_budget() {
        let server = MockServer::start().await;
        let backend = backend_with(&server, STRONG);
        backend.reset_budget(0);

        let error = backend.list().await.unwrap_err();
        assert!(matches!(error, SyncError::Budget), "{error}");
    }

    #[tokio::test]
    async fn oversized_response_is_stopped_while_streaming() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/flashnote/notes/2026-10-23.md"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![
                b'a';
                (MAX_FILE_BYTES + 1)
                    as usize
            ]))
            .mount(&server)
            .await;

        let error = backend_with(&server, STRONG)
            .get("2026-10-23.md")
            .await
            .unwrap_err();
        assert!(matches!(error, SyncError::TooLarge), "{error}");
    }

    #[tokio::test]
    async fn probe_records_strong_etag_and_if_match_support() {
        let server = MockServer::start().await;
        for dir in ["/flashnote/", "/flashnote/notes/"] {
            Mock::given(method("MKCOL"))
                .and(path(dir))
                .respond_with(ResponseTemplate::new(201))
                .mount(&server)
                .await;
        }
        Mock::given(method("PUT"))
            .and(path("/flashnote/notes/.flashnote-probe"))
            .and(header("if-match", "\"flashnote-probe-mismatch\""))
            .respond_with(ResponseTemplate::new(412))
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/flashnote/notes/.flashnote-probe"))
            .and(header("if-none-match", "*"))
            .respond_with(ResponseTemplate::new(412))
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/flashnote/notes/.flashnote-probe"))
            .respond_with(ResponseTemplate::new(201).insert_header("etag", "\"probe-1\""))
            .mount(&server)
            .await;
        Mock::given(method("PROPFIND"))
            .and(path("/flashnote/notes/"))
            .respond_with(
                ResponseTemplate::new(207).set_body_string(multistatus(&[response_xml(
                    "/flashnote/notes/.flashnote-probe",
                    Some("\"probe-1\""),
                    20,
                )])),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/flashnote/notes/.flashnote-probe"))
            .respond_with(
                ResponseTemplate::new(200).set_body_bytes(b"flashnote sync probe".to_vec()),
            )
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/flashnote/notes/.flashnote-probe"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let backend = WebDavBackend::connect(config_for(&server)).await.unwrap();
        let caps = backend.capabilities();
        assert!(caps.strong_etag);
        assert!(caps.honors_if_match);
        assert!(caps.honors_if_none_match);
        assert!(caps.reliable_listing);
        assert!(caps.supports_deletion());
    }

    #[tokio::test]
    async fn probe_degrades_when_etag_and_if_match_are_missing() {
        let server = MockServer::start().await;
        for dir in ["/flashnote/", "/flashnote/notes/"] {
            Mock::given(method("MKCOL"))
                .and(path(dir))
                .respond_with(ResponseTemplate::new(405))
                .mount(&server)
                .await;
        }
        Mock::given(method("PUT"))
            .and(path("/flashnote/notes/.flashnote-probe"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        Mock::given(method("PROPFIND"))
            .and(path("/flashnote/notes/"))
            .respond_with(
                ResponseTemplate::new(207).set_body_string(multistatus(&[response_xml(
                    "/flashnote/notes/.flashnote-probe",
                    None,
                    20,
                )])),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/flashnote/notes/.flashnote-probe"))
            .respond_with(
                ResponseTemplate::new(200).set_body_bytes(b"flashnote sync probe".to_vec()),
            )
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/flashnote/notes/.flashnote-probe"))
            .respond_with(ResponseTemplate::new(405))
            .mount(&server)
            .await;

        let backend = WebDavBackend::connect(config_for(&server)).await.unwrap();
        let caps = backend.capabilities();
        assert!(!caps.strong_etag);
        assert!(!caps.honors_if_match);
        assert!(!caps.honors_if_none_match);
        assert!(!caps.supports_deletion());
    }

    #[tokio::test]
    async fn truncated_listing_aborts_instead_of_reporting_deletions() {
        let server = MockServer::start().await;
        Mock::given(method("PROPFIND"))
            .and(path("/flashnote/notes/"))
            .respond_with(
                ResponseTemplate::new(207).set_body_string(
                    r#"<?xml version="1.0"?><D:multistatus xmlns:D="DAV:"><D:response><D:href>/flashnote/notes/2026-10-23.md</D:href></D:response>"#,
                ),
            )
            .mount(&server)
            .await;

        let error = backend_with(&server, STRONG).list().await.unwrap_err();
        assert!(matches!(error, SyncError::Unsupported(_)));
    }

    #[tokio::test]
    async fn non_ascii_credentials_are_sent_as_utf8() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/flashnote/notes/2026-10-23.md"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(Vec::new()))
            .mount(&server)
            .await;

        let mut backend = WebDavBackend::build(WebDavConfig {
            url: server.uri(),
            username: "张三".into(),
            password: "密码".into(),
            allow_insecure_http: false,
        })
        .unwrap();
        backend.caps = STRONG;
        backend.get("2026-10-23.md").await.unwrap();

        let requests = server.received_requests().await.unwrap();
        let sent = requests[0]
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap();
        let expected = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode("张三:密码".as_bytes())
        );
        assert_eq!(sent, expected);
    }

    /// 对真实服务器跑一遍最小闭环。默认忽略，需要时指定环境变量运行：
    ///
    /// ```text
    /// FLASHNOTE_DAV_URL=http://127.0.0.1:5000/ FLASHNOTE_DAV_USER=... FLASHNOTE_DAV_PASS=... \
    ///   cargo test --locked webdav_round_trip_against_real_server -- --ignored
    /// ```
    #[tokio::test]
    #[ignore = "需要真实的 WebDAV 服务"]
    async fn webdav_round_trip_against_real_server() {
        let (Ok(url), Ok(username), Ok(password)) = (
            std::env::var("FLASHNOTE_DAV_URL"),
            std::env::var("FLASHNOTE_DAV_USER"),
            std::env::var("FLASHNOTE_DAV_PASS"),
        ) else {
            panic!("请设置 FLASHNOTE_DAV_URL / FLASHNOTE_DAV_USER / FLASHNOTE_DAV_PASS");
        };

        let backend = WebDavBackend::connect(WebDavConfig {
            url,
            username,
            password,
            allow_insecure_http: true,
        })
        .await
        .unwrap();

        let body = b"- 10:00 real server\n".to_vec();
        let version = backend
            .put("2099-01-01.md", &body, Expect::Absent)
            .await
            .unwrap();
        let listed = backend.list().await.unwrap();
        assert!(listed.iter().any(|meta| meta.name == "2099-01-01.md"));

        let (read, _) = backend.get("2099-01-01.md").await.unwrap().unwrap();
        assert_eq!(read, body);

        backend
            .delete(
                "2099-01-01.md",
                Expect::Version {
                    version,
                    sha256: None,
                },
            )
            .await
            .unwrap();
        assert!(backend.get("2099-01-01.md").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn delete_reports_missing_remote_file() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/flashnote/notes/2026-10-23.md"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let error = backend_with(&server, STRONG)
            .delete("2026-10-23.md", Expect::Absent)
            .await
            .unwrap_err();
        assert!(matches!(error, SyncError::NotFound));
    }
}
