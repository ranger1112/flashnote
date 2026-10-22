# 更新日志

格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### 修复

- 设置、欢迎页和历史列表的滚动条改为细窄圆角的半透明样式，去掉了原生灰色轨道与箭头，不再贴着圆角卡片边缘割裂视觉；深色与浅色主题分别适配。

### 文档

- 新增 `docs/code-signing.md`：对比不签名、Azure Artifact Signing、OV/EV 证书的成本、SmartScreen 行为与 CI 接入方式，说明 Authenticode 与自动更新 minisign 签名的关系，并给出推荐与后续接入步骤。

## [0.2.1] - 2026-10-22

### 修复

- 关闭「自动检查更新」后会立即取消已排队的检查，不必等到重启。
- 常驻运行的实例每 6 小时复查一次更新，而不是只在启动时查一次。

## [0.2.0] - 2026-10-22

### 新增

- 打 `v*` tag 自动构建并发布：NSIS 安装包、便携版 zip 和 `SHA256SUMS.txt` 一并上传，asset 名统一为 ASCII。
- 自动更新：启动后静默检查新版本，窗口底部提示条可一键下载、校验签名并安装；设置里可关闭自动检查、查看当前版本或手动检查。
- Release 同时生成 `.sig` 与 `latest.json`（minisign 签名），更新包签名不匹配时拒绝安装。
- 历史面板分页加载（每次 100 条）与跨文件全文搜索（正文、时间、来源，不区分大小写）。
- 历史条目 Markdown 渲染预览，开关状态会记住。
- 历史条目可直接编辑与删除；改写当天文件前自动备份为 `<日期>.md.bak-<时间戳>`（每个日期保留 5 份）。

## [0.1.0] - 2026-10-21

首个可用版本。

### 新增

- 全局快捷键 `Ctrl+Alt+N` 唤起 / 收起无边框悬浮小窗。
- `Ctrl+Enter` / `Ctrl+S` 保存，按日期追加到本地 Markdown 文件。
- 自动记录前台窗口标题作为来源（可关闭）。
- 常驻模式（图钉）、草稿自动保存、Esc 收起。
- 可选 Markdown 模式：每条写成 `### 时间` + 原文，块级语法可正常渲染。
- 笔记历史面板：按日期倒序浏览、按关键词过滤。
- 设置：快捷键录制、笔记目录选择、来源抓取、Markdown 模式、开机自启。
- 系统托盘菜单、单实例运行、跟随系统深浅色主题、多显示器边界校正。

[Unreleased]: https://github.com/ranger1112/flashnote/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/ranger1112/flashnote/releases/tag/v0.2.1
[0.2.0]: https://github.com/ranger1112/flashnote/releases/tag/v0.2.0
[0.1.0]: https://github.com/ranger1112/flashnote/releases/tag/v0.1.0
