# 闪记云同步协议 v1

本文定义「闪记云」的客户端 ↔ 服务端契约：认证、密钥、文件版本与冲突处理。

**设计前提**

- 服务端只保存密文，**无法解密任何笔记**；合并永远在客户端完成。
- 账号用邮箱 + 一次性验证码登录；解密用独立的「同步密码」，永不上传。
- 同步单位是按日 Markdown 文件，冲突按条目合并，不按整文件覆盖。
- WebDAV 模式不适用本文的认证与加密部分：它把明文写进用户自己的存储，其余同步规则相同。

协议版本：`1`。所有路径以 `/v1` 开头。

---

## 1. 术语

| 术语 | 含义 |
| --- | --- |
| vault | 一个用户的全部同步数据 |
| name | 文件名，形如 `2026-10-23.md` |
| version | 单个文件的单调递增整数，从 1 开始 |
| seq | vault 级单调递增游标，每次文件变更 +1 |
| tombstone | 删除标记；`version` 继续递增，保留 90 天 |
| key bundle | 服务端保存的加密密钥材料，仅客户端可解开 |
| dataKey | 32 字节随机数据密钥，所有文件用它加密 |
| 同步密码 | 用户设置的本地口令，只用于派生 KEK |

---

## 2. 编码与约束

- JSON 中的二进制字段一律使用 **Base64url 无填充**（RFC 4648 §5）。
- 时间使用 RFC 3339 UTC，例如 `2026-10-23T06:00:00Z`。
- `userId`、`sessionId` 为 UUIDv4 字符串。
- 文件名：

  ```
  ^[0-9]{4}-[0-9]{2}-[0-9]{2}(\.conflict-[a-z0-9-]{1,48})?\.md$
  ```

  服务端额外拒绝：含 `/`、`\`、控制字符、以 `.` 开头、包含 `..`、长度 > 128 的文件名。
- 单文件明文上限 **2 MiB**；密文上限 2 MiB + 64 字节。
- 单 vault 文件数上限 20,000，默认配额 100 MiB（按密文大小计）。
- 邮箱：去首尾空白后整体小写，长度 ≤ 254，仅做基本的 `local@domain` 校验；服务端按 `citext` 唯一。

---

## 3. 密钥

### 3.1 参数

| 项 | 值 |
| --- | --- |
| dataKey | 32 字节，CSPRNG |
| KDF | Argon2id，`m=65536` KiB，`t=3`，`p=1`，输出 32 字节 |
| salt | 16 字节，CSPRNG；每次改密码重新生成 |
| 同步密码 | UTF-8 原始字节，10–1024 字节，**不做 Unicode 归一化、不去空白** |
| 恢复码 | 32 字节 CSPRNG，Crockford Base32 编码为 52 字符，每 4 字符一组 |
| AEAD | XChaCha20-Poly1305，24 字节随机 nonce，16 字节 tag |

同步密码只在客户端使用。服务端不接收、不校验、不记录。

### 3.2 Key bundle

客户端首次启用时生成并上传：

```json
{
  "version": 1,
  "kdf": { "alg": "argon2id", "m": 65536, "t": 3, "p": 1, "salt": "<base64url>" },
  "wrappedKey": "<base64url>",
  "recoveryWrapped": "<base64url>",
  "updatedAt": "2026-10-23T06:00:00Z"
}
```

- `KEK = Argon2id(同步密码, salt, m, t, p)`；
- `wrappedKey = nonce || XChaCha20-Poly1305(KEK, dataKey, aad = aadKeyBundle)`；
- `recoveryKEK = HKDF-SHA256(ikm = recoveryBytes, salt = userId 的 UTF-8 字节, info = "flashnote/recovery-kek/v1", L = 32)`；
- `recoveryWrapped = nonce || XChaCha20-Poly1305(recoveryKEK, dataKey, aad = aadRecovery)`。

AAD 的字节布局（`||` 表示拼接，`0x00` 是单字节分隔符）：

```
aadKeyBundle = "flashnote/key-bundle/v1" || 0x00 || userId
aadRecovery  = "flashnote/recovery/v1"   || 0x00 || userId
```

解开失败（AEAD 校验不通过）即视为密码错误或数据损坏；不需要额外的校验字段。

**改密码**：用旧密码解出 `dataKey`，换新 salt 和 KEK 重新包装，上传新的 key bundle。文件不需要重新加密。

**找回**：用恢复码解出 `dataKey`，设置新同步密码，重新包装。恢复码本身不变。

`kdf` 字段整体透传保存，便于以后升级参数；服务端不解析其内容。

### 3.3 文件加密

```
ciphertext = nonce(24B) || XChaCha20-Poly1305(dataKey, plaintext, aad = aadFile)

aadFile = "flashnote/file/v1" || 0x00 || userId || 0x00 || name 的 UTF-8 字节
```

明文是 UTF-8 编码的 Markdown 原文。服务端只看到 `ciphertext`。

---

## 4. 认证

### 4.1 登录流程

1. `POST /v1/auth/request-code`，body `{ "email": "..." }`；
2. 服务端发送 6 位数字验证码，**无论邮箱是否存在都返回 204**，避免枚举账号；
3. `POST /v1/auth/verify`，body `{ "email": "...", "code": "123456", "deviceName": "DESKTOP-ABC" }`；
4. 首次验证自动创建用户与 vault；
5. 返回设备 token，客户端存入 Windows 凭据管理器。

验证码规则：

- 有效期 10 分钟，最多尝试 5 次，失败即作废；
- 同一邮箱 60 秒内不可重发，每天最多 20 封；同一 IP 每天最多 100 封；
- 存库形式：`HMAC-SHA256(serverPepper, email || ":" || code)`，不存明文。

设备 token：

- 32 字节随机，Base64url；服务端只存 `SHA-256(token)`；
- 有效期 90 天，每次使用滑动续期（每天最多写一次 `last_seen_at`）；
- 可在设备列表中单独吊销。

### 4.2 账号注销

1. `POST /v1/account/delete/request` 发送注销验证码；
2. `DELETE /v1/account`，请求头 `X-OTP: <code>`；
3. 进入 30 天软删除：立即吊销全部 token、拒绝同步，但数据保留；
4. 30 天内可通过邮箱验证码恢复；超期后物理删除数据库记录与全部密文对象。

---

## 5. HTTP 接口

### 5.1 通用约定

请求头：

```
Authorization: Bearer <deviceToken>
X-FlashNote-Protocol: 1
```

错误响应统一为：

```json
{ "error": { "code": "version_mismatch", "message": "版本冲突", "retryAfter": 0 } }
```

| HTTP | code | 含义 |
| --- | --- | --- |
| 400 | `invalid_request` | 参数或文件名非法 |
| 401 | `unauthorized` | token 缺失/过期/被吊销 |
| 403 | `account_deleted` | 账号处于注销冷静期 |
| 404 | `not_found` | 文件或 key bundle 不存在 |
| 409 | `conflict` | 服务端内部状态冲突 |
| 412 | `version_mismatch` | `If-Match` 不匹配 |
| 413 | `payload_too_large` / `quota_exceeded` | 单文件超限 / 空间不足 |
| 426 | `unsupported_protocol` | 协议大版本不支持 |
| 429 | `rate_limited` | 触发限流，带 `Retry-After` |
| 500 | `internal` | 服务端错误，可重试 |

认证相关限流：每 token 60 请求/分钟；`request-code` 按 §4.1。

### 5.2 认证

| 方法 | 路径 | 请求 | 响应 |
| --- | --- | --- | --- |
| `POST` | `/v1/auth/request-code` | `{email}` | `204` |
| `POST` | `/v1/auth/verify` | `{email, code, deviceName}` | `{token, expiresAt, userId, email, hasKeyBundle}` |
| `GET` | `/v1/auth/me` | — | `{userId, email, usage: {bytes, quota}, createdAt}` |
| `GET` | `/v1/auth/sessions` | — | `[{id, deviceName, createdAt, lastSeenAt, current}]` |
| `DELETE` | `/v1/auth/sessions/{id}` | — | `204` |
| `POST` | `/v1/auth/logout` | — | `204` |

### 5.3 Key bundle

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| `GET` | `/v1/key-bundle` | `200` 返回 bundle；未初始化返回 `404` |
| `PUT` | `/v1/key-bundle` | 上传或覆盖；服务端只校验 JSON 结构与大小（≤ 8 KiB） |

服务端不校验、也无法校验内容正确性。覆盖操作仅需已认证的设备 token。

### 5.4 文件

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| `GET` | `/v1/files?since=<seq>&limit=<n>` | 增量列表，`since` 不含；`limit` 默认 200，最大 500 |
| `GET` | `/v1/files/{name}` | 返回密文；响应头 `X-Version`、`X-Seq`、`X-Sha256` |
| `PUT` | `/v1/files/{name}` | 写密文；`If-Match: <version>` 或首次写入 `If-None-Match: *` |
| `DELETE` | `/v1/files/{name}` | 写墓碑；必须带 `If-Match` |

列表响应：

```json
{
  "seq": 128,
  "hasMore": false,
  "files": [
    {
      "name": "2026-10-23.md",
      "version": 7,
      "seq": 126,
      "size": 412,
      "sha256": "<base64url>",
      "deleted": false,
      "updatedAt": "2026-10-23T06:00:00Z"
    }
  ]
}
```

- `seq` 为本次响应的最大游标；`since` 之后的变更按 `seq` 升序返回；
- `sha256` 是**密文**的 SHA-256，用于下载后校验完整性；
- 删除的文件以 `deleted: true` 返回，保留 90 天；
- 分页期间文件可能再次变更，客户端按 `name` 去重并取最新一条。

`PUT` 响应：

```json
{ "version": 8, "seq": 129, "sha256": "<base64url>" }
```

`version_mismatch` 响应附带当前状态：

```json
{ "error": { "code": "version_mismatch", "message": "版本冲突" }, "current": { "version": 7, "seq": 126 } }
```

语义：

- 写入必须原子地检查 expected version；不匹配返回 412，不产生任何修改；
- 对墓碑执行 `PUT` 且 `If-Match` 匹配墓碑版本时，视为「复活」，清除 `deleted_at` 并递增版本；
- 幂等重试：客户端收到 412 时，若 `sha256` 与待上传内容一致，可直接视为成功；
- 配额检查与版本检查在同一事务内完成。

---

## 6. 服务端数据

```sql
users(id uuid primary key, email citext unique not null,
      created_at timestamptz not null default now(), deleted_at timestamptz)

sessions(id uuid primary key, user_id uuid not null references users(id) on delete cascade,
         token_hash bytea not null, device_name text not null,
         created_at timestamptz not null default now(),
         last_seen_at timestamptz not null default now(),
         expires_at timestamptz not null, revoked_at timestamptz)

otp_codes(email citext not null, code_hash bytea not null,
          expires_at timestamptz not null, attempts int not null default 0,
          created_at timestamptz not null default now())

key_bundles(user_id uuid primary key references users(id) on delete cascade,
            salt bytea not null, wrapped_key bytea not null,
            recovery_wrapped bytea not null, kdf_params jsonb not null,
            updated_at timestamptz not null default now())

vaults(user_id uuid primary key references users(id) on delete cascade,
       quota_bytes bigint not null default 104857600,
       seq bigint not null default 0,
       created_at timestamptz not null default now())

files(vault_id uuid not null references vaults(user_id) on delete cascade,
      name text not null, version bigint not null, seq bigint not null,
      size bigint not null, sha256 bytea not null,
      deleted_at timestamptz, updated_at timestamptz not null default now(),
      primary key (vault_id, name))

create index files_seq_idx on files (vault_id, seq);
```

对象存储布局（本地磁盘或 S3 兼容存储）：

```
blobs/<userId>/<name>/<version>
```

- 每个文件保留最近 3 个版本、最长 30 天，其余由 GC 清理；
- 写入顺序：先落对象并 `fsync`，再提交数据库事务；事务失败留下的孤儿对象由 GC 回收；
- 墓碑对应的对象按保留策略延后删除。

---

## 7. 客户端同步算法

### 7.1 本地状态

保存在 `app_config_dir()/sync/<哈希(笔记目录 + 远端身份)>/`。远端身份对闪记云是 `userId`，对 WebDAV 是「URL + 用户名」；换账号或换服务器等于换一份状态，首轮只上传/合并，不会把本地文件误判成「远端已删除」。

```json
{
  "version": 1,
  "userId": "…",
  "lastSeq": 128,
  "files": {
    "2026-10-23.md": {
      "localMtimeNs": 1761200000000000000,
      "localSize": 380,
      "localSha256": "<base64url>",
      "remoteVersion": 7,
      "remoteSha256": "<base64url>",
      "syncedAt": "2026-10-23T06:00:00Z",
      "deletedAt": null
    }
  }
}
```

同时保存 `base/<name>`（上次同步成功时的明文）与 `trash/<时间戳>/`（远端删除时的本地副本）。

### 7.2 单轮流程

1. 列出本地 `*.md`（严格 UTF-8、≤ 2 MiB），计算 SHA-256；
2. `GET /v1/files?since=lastSeq` 拉增量；若返回的 `seq` 小于 `lastSeq`，判定云端回滚，**中止并提示**；
3. 对每个 `name` 做三方比较（本地、远端、state）；
4. 按 §7.3 执行上传/下载/合并/删除；
5. 每成功一个文件就更新 state；整轮结束时写入 `lastSeq`；
6. 任何网络失败保留 state，下一轮从上次进度继续。

### 7.3 决策表

| 本地 | 远端 | 处理 |
| --- | --- | --- |
| 未变 | 未变 | 跳过 |
| 变了 | 未变 | 上传 |
| 未变 | 变了 | 下载 → 校验 sha256 → 解密 → 临时文件 + rename |
| 变了 | 变了 | 下载 → 三方合并 → 写回本地 → 上传 |
| 新增 | 不存在 | 上传 |
| 不存在 | 新增 | 下载 |
| 删除 | 未变 | `DELETE` 写墓碑 |
| 删除 | 已变 | 不删；把远端恢复回本地并提示 |
| 未变 | 墓碑 | 本地移入 `trash/` |
| 已变 | 墓碑 | 以本地为准，`PUT` 复活 |

安全阀：

- 笔记目录不存在或不可读 → 整轮中止；
- 单轮拟删除 > 5 个且占已同步文件 30% → 中止并提示；已跟踪文件在 >= 3 个的前提下全部同时消失（目录被清空、网络盘未挂载）→ 同样中止；
- 本地文件非 UTF-8 或超过 2 MiB → 跳过并警告，绝不上传有损解码内容；
- `userId` 变化、切换账号 → 使用独立 state 目录，不触发删除。

### 7.4 条目级三方合并

基于 `src-tauri/src/notes.rs` 的 `split_spans` 按**行区间**拼接，不重新渲染整份文件，CRLF、连续空行与缺失的末尾换行都保持原样：

1. **可合并性检查**：三份内容按行区间切分；出现无法归入条目的非空行，或条目索引无法一一匹配时放弃合并，保留本地文件，把远端保存为 `<日期>.conflict-<设备>-<时间>.md`；
2. **条目身份**：`(时间, 正文, 来源)` 的多重集；
3. 计算 base → local 的删除集与新增集，施加到远端条目：
   - 本地删除、远端未改 → 删除；
   - 本地新增 → 按 `HH:MM` 插入；
   - 双方新增 → 并集去重；
   - 同一条目两边改成不同内容 → 两个版本都保留，冲突计数 +1；
4. 文件头：本地非默认头优先，否则取远端；
5. 合并结果写回本地并上传，同时更新 `base/`。

冲突不阻塞同步；UI 汇总提示「已合并 N 处并发修改」。

### 7.5 触发与重试

- 保存/删除/编辑后 debounce 5 秒；
- 启动 60 秒后首轮；之后每 5/15/60 分钟；
- 手动同步与托盘入口；
- 单飞：同一时刻只允许一轮，运行中的触发记一个 pending，结束后补跑；
- 退避：单次请求内对网络错误、`423`、`429`、`503` 自动重试两次（1s、5s）；`Retry-After` 按服务端给的秒数等待，单轮最多 30 秒；
- `401` 停止整轮并提示检查密码；`412` 不重试，下一轮按新版本重新比对或合并；
- 退出不等待同步，但要求 state 已落盘。

### 7.6 线程与锁

- 扫描、哈希、加解密、合并、写回全部在 `spawn_blocking` 中执行；
- 网络请求为纯 async；
- 本地写文件时持有 `files::io_guard`，且**不跨 await**；
- `dataKey` 只存在于 Rust 侧内存与 Windows 凭据管理器，不进入 WebView、不写日志。

### 7.7 WebDAV 差异

- 远端布局固定为 `<根>/flashnote/notes/<name>`，不写其它前缀；
- 删除即对文件发 `DELETE`，没有墓碑；发删除前先把 base 副本（缺失时回读远端）写入本轮 `trash/`，保证两端都还能找回原文；是否传播删除取决于能力探测：强 ETag、`If-Match` 真的返回 412、目录列表完整可解析，三者同时成立才允许删除，否则该轮只上传/下载；
- 能力不足时本地删除不会发往远端；客户端保留该文件的同步记录，不会把远端副本重新下载回来，直到远端内容发生变化或被删除；
- 弱 ETag 服务器用「`Last-Modified` + 内容长度」派生版本号；`If-Match` 不生效时每次写入后回读比对；
- 能力探测分别验证 `If-Match` 与 `If-None-Match`：后者不生效时不发送该头，改为写前确认远端不存在、写后回读，避免并发创建时静默覆盖；
- 写请求（PUT/DELETE）遇到 `303` 视为失败：303 只对 GET/HEAD 跟随，否则会把没有发生的写入当成成功；
- 列表响应被截断或解析失败时整轮中止，不做任何删除；
- 文件为明文，§3–§5 的账号、密钥与加密接口不适用。

---

## 8. 安全要求

服务端：

- 仅允许 TLS 1.2+，启用 HSTS；数据库不暴露公网；
- token、验证码只存哈希，比较使用常量时间算法；
- 不记录请求体、`Authorization`、`X-OTP`；访问日志保留 ≤ 30 天，IP 仅用于限流，保留 ≤ 7 天；
- 请求体大小、连接数、超时均设上限；
- 依赖最小权限运行，数据库账号只访问本库；
- 每晚加密备份，定期恢复演练。

客户端：

- 上传前确认文件名匹配 §2 规则；
- 下载后先校验密文 SHA-256 再解密；
- 检测 `seq` / `version` 回退并告警；
- 同步密码与恢复码不出现在日志、崩溃报告、剪贴板历史提示中。

已知元数据泄露：邮箱、文件名（日期）、文件大小、修改时间。文件名如后续需要隐藏，可在 v2 引入确定性加密（SIV），届时 `name` 改为不透明标识，列表由客户端映射。

---

## 9. 兼容性

- 路径级大版本：`/v1`；不兼容变更走 `/v2`，服务端至少并行支持一个大版本 12 个月；
- 客户端发送 `X-FlashNote-Protocol: 1`，服务端返回 `426` 表示不支持；
- JSON 新增字段必须可忽略；删除或改变语义的字段升 `keyBundle.version` 或协议大版本；
- 密文格式通过 AAD 中的版本串区分，不依赖外部字段。

---

## 10. 实现清单

服务端：

- [ ] 邮箱验证码、会话、限流
- [ ] key bundle 存取
- [ ] 文件 CRUD、版本、墓碑、增量列表
- [ ] 配额与单文件上限
- [ ] 对象落盘、GC、备份
- [ ] 账号注销与冷静期
- [ ] 监控与告警

客户端：

- [ ] `CloudBackend`（HTTP + token）
- [ ] Argon2id、XChaCha20-Poly1305、恢复码
- [ ] keyring 存取 `dataKey` 与 device token
- [ ] 登录/验证码/同步密码/恢复码/设备列表 UI
- [ ] 复用现有同步引擎与三方合并
- [ ] 回滚检测、错误提示、注销流程

测试：

- [ ] 固定向量：Argon2id 派生、key bundle 包装、文件加密
- [ ] 服务端并发写入与 412 路径
- [ ] 客户端双设备并发、删除传播、回滚检测
- [ ] 限流、验证码错误、过期 token

---

## 11. 待定

- 免费配额是否维持 100 MiB；
- 冲突副本是否作为普通文件同步（当前：是）；
- 是否在 v1 就隐藏文件名；
- 注销冷静期是否可缩短；
- 邮件服务商与发信域；
- 收费与经营主体（影响国内合规路径）。
