# 闪记 FlashNote

**中文** | [English](README.en.md)

> 看文章、看视频时不打断思路的桌面速记小窗。

按一下全局快捷键，小窗浮在当前窗口上方；写下想法，`Ctrl+Enter` 保存，窗口自动收起，焦点回到原来的文章或播放器。内容按日期追加到本地 Markdown 文件，不注册账号、不上传云端。

**下载**：[最新版本](https://github.com/ranger1112/flashnote/releases/latest)

| 小窗（浅色） | 小窗（深色） |
| --- | --- |
| ![浅色主题小窗](docs/screenshot-light.png) | ![深色主题小窗](docs/screenshot-dark.png) |
| ![笔记历史](docs/history.png) | ![设置面板](docs/settings.png) |

## 特性

- **全局唤起**：默认 `Ctrl+Alt+N`，在任何程序里都能叫出小窗，再按一次收起。
- **无边框悬浮**：始终置顶、不占任务栏，可自由拖动和缩放，位置与尺寸自动记住。
- **两种保存方式**：默认「保存即收起」；点图钉按钮进入常驻模式，保存后窗口保持打开。
- **自动记录来源**：唤起时抓取前台窗口标题，保存时写入条目（Windows，可在设置中关闭）。
- **草稿不丢**：`Esc` 收起时保留未保存内容，重启应用后仍然存在；草稿写在应用配置目录，清理浏览器数据也不会丢。
- **本地优先**：Markdown 按天分文件，可直接用 Obsidian、VS Code 等查看。
- **可选云同步**：用自己的 WebDAV 空间同步笔记目录（坚果云 / Nextcloud / NAS 等）；默认关闭，不配置不联网。两端同时修改按条目合并，无法合并时保留冲突副本。
- **Markdown 模式**：可选按块写入，代码块、表格、标题从第一行起即可正常渲染。
- **笔记历史**：标题栏的笔记按钮打开历史面板，按日期倒序浏览、全文搜索（跨所有日期文件）、加载更早的记录；可切换 Markdown 渲染预览，也能直接编辑或删除单条记录。
- **系统托盘**：左键唤起；右键可打开今日笔记、笔记文件夹、设置或退出。
- **单实例运行**：重复启动只会唤起已有窗口，不会出现第二个托盘图标或热键冲突。
- **自动更新**：启动后静默检查 GitHub Release；发现新版本时窗口底部出现提示条，点击即可下载、校验签名并安装，重启后生效。
- **外观**：主题可选（跟随系统 / 浅色 / 深色），背景不透明度可调（60%–100%）。
- **其他**：可选开机自启、失去焦点时自动收起、多显示器边界校正。

## 使用

| 操作 | 说明 |
| --- | --- |
| `Ctrl+Alt+N` | 全局唤起 / 收起（可在设置里修改） |
| `Ctrl+Enter` 或 `Ctrl+S` | 保存当前内容 |
| `Esc` | 收起窗口；设置面板打开时先关闭面板 |
| 笔记图标 | 打开笔记历史面板；支持搜索、加载更多、预览、编辑与删除 |
| 图钉按钮 | 切换常驻模式：保存后不关闭窗口 |
| 拖动标题栏 | 移动窗口，位置自动记住 |
| 右下角手柄 | 拖动调整窗口大小（最小 380×240） |

保存时内容为空则直接收起，不会写入文件。非常驻模式下，保存成功后窗口自动收起，焦点回到之前的程序。

首次运行时会自动弹出引导，说明唤起快捷键、笔记保存位置和图钉的作用；点「开始使用」后不再出现，之后也可以从设置里重新查看这些信息。

### 托盘菜单

左键点击托盘图标直接唤起小窗；右键菜单包含：

- **显示速记**：同全局快捷键。
- **查看笔记**：唤起窗口并打开笔记历史面板。
- **打开今日笔记**：用系统默认关联程序打开当天的 Markdown 文件（通常是 VS Code / Typora；文件不存在时自动创建）。
- **打开笔记文件夹**：打开当前笔记目录。
- **立即同步**：立刻跑一轮云同步；未配置或已关闭时打开设置页。
- **设置…**：唤起窗口并打开设置面板。
- **打开日志文件夹**：打开日志目录，排障时使用。
- **退出**：退出应用并释放全局热键。

## 设置

| 选项 | 说明 |
| --- | --- |
| 唤起快捷键 | 点击「录制」后按下组合键；必须包含至少一个修饰键（Ctrl / Alt / Shift / Win）。冲突时显示错误并自动恢复旧快捷键；启动时若默认快捷键已被占用，会自动改用备用组合并在窗口内提示。 |
| 笔记目录 | 通过文件夹选择器更换；目录不存在会自动创建。 |
| 自动记录来源窗口 | 关闭后不再抓取和写入来源标题。 |
| Markdown 模式 | 开启后每条写成 `### 时间` + 原文，代码块、表格等块级语法可正常渲染；默认关闭。 |
| 开机自启 | 写入当前用户的启动项，默认关闭。 |
| 自动检查更新 | 启动后静默检查新版本，常驻运行时每 6 小时复查，默认开启；关闭后立即停止排队的检查，仍可用「检查更新」手动触发。 |
| 失去焦点时自动收起 | 点击其他窗口后自动收起小窗，默认关闭；常驻模式和面板打开期间不生效。 |
| 主题 | 跟随系统、浅色或深色；跟随系统时随 Windows 深浅色实时切换。 |
| 背景不透明度 | 60%–100%，小窗与面板使用同一透明度；默认 100%（不透明）。 |
| 云同步（WebDAV） | 启用开关、地址、账号、密码、同步间隔与设备名；默认关闭，详见下方「云同步（WebDAV）」。 |

设置保存在：

```
%APPDATA%\com.phoebe.flashnote\settings.json
```

未保存的草稿保存在同目录的 `draft.txt`，保存笔记成功后自动清空。

写入采用「临时文件 + 重命名」，避免异常退出留下损坏的 JSON。若文件已损坏（例如手动编辑出错），启动时会先备份为 `settings.json.corrupt-<时间戳>`，再恢复默认设置并提示。

## 数据格式

默认保存到系统「文档」目录下的 `FlashNote\YYYY-MM-DD.md`，可在设置里更换目录。首次写入时会创建当天文件：

```markdown
# 2026-10-21

- 14:32 这里是一个想法
  第二行会缩进两格，仍属于同一条
  _来源：Google Chrome — 某篇文章_

- 15:02 另一个问题
  _来源：PotPlayer — 视频文件名_
```

规则：

- 每条以 `- HH:MM ` 开头，时间取本地时间；跨天自动写入新文件。
- 多行内容从第二行起缩进两个空格，保持为同一条。
- 来源行格式为 `  _来源：窗口标题_`，仅在抓取成功时写入；标题中的换行会压成空格。
- 新笔记始终追加写入；在历史面板中编辑或删除时只重写当天的文件，并先生成 `2026-10-21.md.bak-<时间戳>` 备份（每个日期最多保留 5 份）。
- 文件为 UTF-8 无 BOM、`\n` 换行。
- 云同步只处理 `YYYY-MM-DD.md` 与 `YYYY-MM-DD.conflict-*.md`；`.bak-*`、`.tmp-*`、草稿与设置不会上传。

### 历史面板

- 默认每次加载 100 条，底部「加载更多」继续向前翻。
- 搜索会扫描笔记目录下所有 `YYYY-MM-DD.md`，不区分大小写，覆盖正文、时间与来源标题，最多返回 200 条。
- 「预览」按 Markdown 渲染条目内容（经 DOMPurify 清洗）；再点一次回到原文。开关状态会记住。
- 「编辑」直接改写条目并保留来源行与原格式（紧凑列表 / Markdown 块）；「删除」单击即移除，5 秒内可点「撤销」恢复。

### Markdown 模式

开启后，每条改为「时间标题 + 原文」，不再加列表前缀和续行缩进，因此代码块、表格、标题可以从第一行开始正常渲染；来源写入 HTML 注释，阅读视图里不可见：

````markdown
# 2026-10-21

### 14:32
## 小标题

```js
const a = 1;
```

<!-- 来源：VS Code — notes.rs -->
````

该模式默认关闭，可在设置中切换。关闭时使用上面的紧凑列表格式，两种格式可以混用，旧条目不会被改写。

## 云同步（WebDAV）

默认关闭。只使用你自己的 WebDAV 空间，笔记以**明文 Markdown** 写到：

```
<你填写的地址>/flashnote/notes/YYYY-MM-DD.md
```

除这个前缀外不会写入任何路径，也不会上传设置、草稿与 `.bak-*`。

### 配置（以坚果云为例）

1. 登录坚果云网页版 →「账户信息」→「安全选项」→「添加应用密码」，得到专用密码；
2. 打开闪记设置 →「云同步（WebDAV）」；
3. 地址填 `https://dav.jianguoyun.com/dav/`，用户名填坚果云登录邮箱，密码填**应用密码**（不是登录密码）；
4. 点「测试连接」确认可读写，再点「保存」并勾选「启用云同步」。

Nextcloud、NAS 自带 WebDAV、[dufs](https://github.com/sigoden/dufs)、`rclone serve webdav` 等填各自的 DAV 根地址即可。地址只写到目录，不要带文件名。

### 行为

- 触发：保存 / 删除 / 编辑后 5 秒；启动 60 秒后首轮；之后每 5 / 15 / 60 分钟；托盘「立即同步」随时触发。
- 同一时刻只跑一轮；跑批中再次触发会在结束后补跑一轮。
- 本地删除会同步删除远端，删之前先把远端副本存进本地 `trash`；远端删除会把本地副本移进 `%APPDATA%\com.phoebe.flashnote\sync\<哈希>\trash\<时间戳>\`。
- 两端同时改同一天：按条目（时间 + 正文 + 来源）三方合并，双方新增都会保留；无法安全合并时远端内容另存为 `YYYY-MM-DD.conflict-<设备>-<时间>.md`，历史面板会显示「N 个冲突副本」。
- 安全阀：单轮拟删除超过 5 个且超过已同步文件的 30%，或已同步文件（≥3 个）在本地同时消失时中止；笔记目录读不到、远端列表被截断时整轮不做删除。
- 弱服务器（没有强 ETag、忽略条件写入、列表不可靠）自动降级：只上传 / 下载，**不会传播删除**，设置页会标注原因。
- 每轮请求数有上限，超出部分留到下一轮，避免触发服务商限流。

### 凭据与隐私

- 密码只存 Windows 凭据管理器（服务名 `flashnote-sync`）；`settings.json` 里只有地址、用户名与非敏感参数，界面不会回显已保存的密码。
- 凭据管理器被组策略禁用时会明确提示；本次会话仍可同步，退出后需要重新输入。
- **同步内容是明文**：WebDAV 服务商能看到笔记原文。需要端到端加密的「闪记云」计划在 0.6.0。
- 修改地址或用户名会自动清掉旧密码，避免把凭据发给新服务器。

### 排障

- 「认证失败」：坚果云必须使用应用密码；也检查地址是否以 `/dav/` 结尾。
- 「请求过于频繁」：服务商限流，闪记会按 `Retry-After` 退避；可把同步间隔调长，或减少同时使用的客户端。
- 冲突副本不会自动清理；确认内容后手动删除或合并即可。
- 同步状态与失败原因显示在设置页与历史面板底部。

协议细节（决策表、合并规则、状态文件）见 [docs/sync-protocol.md](docs/sync-protocol.md)；WebDAV 模式不涉及其中的账号与加密部分。

## 安装

从 [Releases](https://github.com/ranger1112/flashnote/releases/latest) 下载：

| 版本 | 文件 | 说明 |
| --- | --- | --- |
| 安装版 | `FlashNote_<版本>_x64-setup.exe` | 双击安装，按当前用户安装，无需管理员权限 |
| 便携版 | `FlashNote_<版本>_portable_x64.zip` | 解压后直接运行 `flashnote.exe`，不写安装目录 |

每个 Release 都附带 `SHA256SUMS.txt`，可用来校验下载是否完整：

```powershell
Get-FileHash .\FlashNote_0.5.1_x64-setup.exe -Algorithm SHA256
```

把输出与 `SHA256SUMS.txt` 中对应行比对即可。

安装包未签名，首次运行可能触发 SmartScreen 提示，选择「更多信息 → 仍要运行」即可。签名方案的成本、取舍与接入路径见 [docs/code-signing.md](docs/code-signing.md)。

卸载：Windows「设置 → 应用」，或运行安装目录下的 `uninstall.exe`。

从源码构建的安装包位于：

```
src-tauri\target\release\bundle\nsis\FlashNote_0.5.1_x64-setup.exe
```

## 开发

### 环境要求

- Windows 10 / 11
- Node.js 22.12+ 或 24 LTS
- Rust stable（MSVC 工具链）
- Visual Studio Build Tools 2022，勾选「使用 C++ 的桌面开发」
- WebView2 运行时（Windows 10 1803+ 一般已自带）

### 启动开发环境

```bash
npm install
npm run tauri dev
```

Windows 下也可以使用封装脚本，它会自动找到 Rust 与 MSVC（`vswhere` + 常见安装路径）并加载环境：

```bat
scripts\dev.cmd
scripts\build.cmd
scripts\cargo.cmd test      :: 等价于 cargo test，但环境已就绪
```

工具链装在非默认位置（例如 `D:\BuildTools`、自定义 `CARGO_HOME`）时，把示例复制成 `scripts\_env.local.cmd` 并填写路径即可，该文件已被 `.gitignore` 忽略：

```bat
copy scripts\_env.local.cmd.example scripts\_env.local.cmd
```

### 构建安装包

```bash
npm run tauri build
```

产物：

- 可执行文件：`src-tauri\target\release\flashnote.exe`
- NSIS 安装包：`src-tauri\target\release\bundle\nsis\FlashNote_0.5.1_x64-setup.exe`

### 测试

```bash
npm test                 # 前端单测：纯函数 + happy-dom 状态机
cd src-tauri && cargo test
```

Rust 单测覆盖条目格式化、多行缩进、来源标题归一化、文件追加分隔、分页、搜索，编辑 / 删除后的文件重建，设置的原子写入与损坏恢复，笔记目录的路径越界校验，以及窗口几何的多屏夹取与 DPI 换算；前端用 Vitest 覆盖 `accelFromEvent` / `formatAccel`、搜索匹配等纯函数，并在 happy-dom 里装载真实的 `index.html`，对删除撤销、搜索竞态、设置回滚、历史错误态与草稿加载做 DOM 级回归。两项都由 CI 执行。

### 发版

先统一五处版本号（`package.json`、`package-lock.json`、`src-tauri/Cargo.toml`、`src-tauri/Cargo.lock`、`src-tauri/tauri.conf.json`）：

```bash
npm run bump -- 0.4.1     # 写入新版本（npm 需要 `--` 才能把参数传给脚本）
npm run version:check     # 只校验是否一致，CI 也会执行
```

再补 CHANGELOG、更新 README 里的安装包文件名，然后打 tag：

```bash
git tag v0.4.1
git push origin v0.4.1
```

`.github/workflows/release.yml` 会自动构建 NSIS 安装包与便携版 zip、生成 `SHA256SUMS.txt`，并创建一个 **draft** Release；检查无误后在 GitHub 上点发布。版本号不一致时工作流会直接失败，避免发出 tag 与安装包版本不符的包。

自动更新依赖仓库的两个 Actions secret：`TAURI_SIGNING_PRIVATE_KEY`（minisign 私钥全文）与 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`。工作流会用它们生成 `.sig` 与 `latest.json`；缺少时 Tauri 会直接中止构建，不会产出安装包。私钥只在本地备份，不要提交进仓库。发布后 `latest.json` 的地址是：

```
https://github.com/ranger1112/flashnote/releases/latest/download/latest.json
```

这个地址只指向**已发布**的最新正式版，draft 和 prerelease 不会被自动更新选中。

### 项目结构

```
flashnote/
├─ index.html                  # 小窗与设置面板的 DOM 结构
├─ src/
│  ├─ main.ts                  # 入口与编排：启动、保存、显隐、缩放、全局键盘
│  ├─ dom.ts                   # DOM 元素引用
│  ├─ panels.ts                # 面板显隐、焦点圈定与背景 inert
│  ├─ history.ts               # 笔记历史：列表、搜索、分页、编辑、删除撤销
│  ├─ settings-panel.ts        # 设置面板与快捷键录制
│  ├─ settings-store.ts        # 设置内存状态与读取
│  ├─ appearance.ts            # 主题与背景不透明度
│  ├─ draft.ts                 # 草稿自动保存与恢复
│  ├─ sync.ts                  # 云同步设置区、状态订阅与冲突提示
│  ├─ updater.ts               # 更新检查、安装与提示横幅
│  ├─ welcome.ts               # 首次使用引导
│  ├─ toast.ts                 # 提示与撤销条
│  ├─ lib.ts                   # 纯函数：快捷键解析、搜索匹配、分页偏移
│  ├─ lib.test.ts              # 前端纯函数单元测试（Vitest）
│  ├─ main.dom.test.ts         # 前端状态机 DOM 测试（happy-dom）
│  └─ styles.css               # 深/浅色主题、悬浮卡片样式
├─ src-tauri/
│  ├─ src/
│  │  ├─ main.rs               # 应用入口：插件、托盘、单实例、热键注册
│  │  ├─ commands.rs           # 前后端命令：显隐、保存、设置
│  │  ├─ files.rs              # 打开文件/目录、URL 白名单、笔记改写（备份 + 原子替换）
│  │  ├─ geometry.rs           # 窗口几何：多屏夹取、DPI 换算、位置恢复
│  │  ├─ notes.rs              # Markdown 条目格式化与按日追加（含测试）
│  │  ├─ settings.rs           # settings.json 原子读写与默认值
│  │  ├─ source.rs             # 抓取前台窗口标题（Win32）
│  │  ├─ sync/
│  │  │  ├─ mod.rs             # 同步引擎：扫描、决策表、冲突副本、安全阀
│  │  │  ├─ backend.rs         # 后端抽象：版本、前置条件、错误与能力
│  │  │  ├─ webdav.rs          # WebDAV 客户端、能力探测与 XML 解析
│  │  │  ├─ state.rs           # state.json、base 副本与 trash
│  │  │  ├─ merge.rs           # 条目级三方合并（按行区间拼接）
│  │  │  ├─ runtime.rs         # 调度、单飞与状态广播
│  │  │  └─ secrets.rs         # 密码读写（Windows 凭据管理器）
│  │  └─ tray.rs               # 托盘图标与菜单
│  ├─ capabilities/default.json
│  ├─ icons/
│  ├─ tauri.conf.json
│  └─ Cargo.toml
├─ scripts/                    # 开发/构建封装脚本与调试工具
└─ package.json
```

## 技术说明

- **框架**：Tauri 2 + Rust 2021，前端为原生 TypeScript + Vite，不使用前端框架。
- **插件**：`single-instance`（单实例）、`global-shortcut`（全局热键）、`autostart`（开机自启）、`dialog`（目录选择）、`updater` / `process`（更新检查、安装与重启）。
- **前后端边界**：前端不直接访问文件系统，保存、设置、窗口显隐全部通过 Tauri command 完成。
- **窗口**：无边框 + 透明背景，四周留 10px 由 CSS 绘制圆角与阴影；`skipTaskbar` 使其不占用任务栏。
- **来源抓取**：`GetForegroundWindow` + `GetWindowTextW`，通过 PID 排除闪记自身；抓不到时沿用上一次来源。
- **保存**：按日期文件追加写入，设置文件原子替换；文件 I/O 与目录创建都在阻塞线程池执行，设置异步落盘且只保留最新快照，UI 线程不等磁盘。
- **云同步**：`reqwest`（系统证书）+ `quick-xml` 解析 PROPFIND；同步状态按「笔记目录 + 远端身份」隔离，换服务器不会复用旧记录；密码由 `keyring` 存入 Windows 凭据管理器，不落明文。
- **日志**：写入 `%LOCALAPPDATA%\com.phoebe.flashnote\logs`，单个文件超过 2MB 轮转、保留最近 3 份；托盘菜单可直接打开。日志不记录笔记正文。

## 平台支持

目前仅在 **Windows 10/11** 上验证。来源窗口抓取依赖 Win32 API，其他平台会省略来源行；macOS / Linux 未做测试。

## 已知限制

- 暂无导出功能；搜索为逐文件扫描，笔记量很大时会稍慢。
- 云同步只支持 WebDAV，内容以明文存放在你自己的存储里；暂无端到端加密与实时推送。
- 全局快捷键被其他程序占用时需要在设置里更换。
- 笔记文件只追加不轮转，长期使用后单日文件会逐渐变大。
- 安装包未做代码签名，首次安装会触发 SmartScreen 提示；调研结论与后续接入步骤见 [docs/code-signing.md](docs/code-signing.md)。

## 常见问题

**按了快捷键没反应？**

多半是被其他程序占用了。启动时如果默认快捷键注册失败，闪记会依次尝试 `Ctrl+Alt+M`、`Ctrl+Shift+Alt+N`，并在窗口里提示。也可以右键托盘图标 → 设置，点「录制」换一个组合；快捷键必须包含 Ctrl / Alt / Shift / Win 中的至少一个。

**笔记存在哪里？**

默认在 `%USERPROFILE%\Documents\FlashNote`，每个日期一个 Markdown 文件；设置里可以改目录，托盘菜单可直接打开。草稿和设置存放在 `%APPDATA%\com.phoebe.flashnote`，日志在 `%LOCALAPPDATA%\com.phoebe.flashnote\logs`。

**怎么卸载？**

Windows「设置 → 应用」里卸载「FlashNote」（闪记），或运行安装目录（`%LOCALAPPDATA%\FlashNote`）下的 `uninstall.exe`。笔记目录不会被删除，需要时手动清理即可。

**为什么 Windows 提示「Windows 已保护你的电脑」？**

安装包没有做代码签名，SmartScreen 对下载量少的新程序会拦截：点「更多信息」→「仍要运行」即可。也可以用 Release 里的 `SHA256SUMS.txt` 校验文件完整性。

**升级会动我的笔记吗？**

不会。自动更新只替换程序文件；笔记、设置和草稿都在用户目录下，升级和卸载都不会碰它们。

**关闭「自动检查更新」后还会联网吗？**

不会主动联网。关闭后只有手动点「检查更新」时才会访问 GitHub Releases；云同步默认关闭，同样不会发起任何网络请求。

**云同步会把我的笔记发到哪里？**

只发到你自己填写的 WebDAV 地址下的 `flashnote/notes/` 目录，内容为明文 Markdown。闪记没有云端服务器，也不会把笔记发往其它地方。换电脑时，在另一台机器上填同一地址与账号，即可把笔记拉下来。

**两台电脑同时改同一天会丢笔记吗？**

不会整文件覆盖。闪记按条目合并双方的新增与修改；实在无法安全合并时，保留本地版本并把远端内容存成 `YYYY-MM-DD.conflict-<设备>-<时间>.md`，历史面板底部的「N 个冲突副本」可以直接打开文件夹查看。

## 参与贡献

欢迎提交 Issue 和 Pull Request，具体流程见 [CONTRIBUTING.md](CONTRIBUTING.md)。提交前请确认：

- `npm run build`（类型检查 + 前端构建）通过；
- `npm test`（前端单元测试）通过；
- `cd src-tauri && cargo test` 通过；
- 不提交 `node_modules/`、`dist/`、`src-tauri/target/` 等生成目录。

版本变更见 [CHANGELOG.md](CHANGELOG.md)；安全问题请按 [SECURITY.md](SECURITY.md) 私下报告，不要开公开 Issue。

## 许可证

本项目基于 [MIT License](LICENSE) 开源。
