# 更新日志

格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### 新增

- 打 `v*` tag 自动构建并发布：NSIS 安装包、便携版 zip 和 `SHA256SUMS.txt` 一并上传，asset 名统一为 ASCII。
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

[Unreleased]: https://github.com/ranger1112/flashnote/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/ranger1112/flashnote/releases/tag/v0.1.0
