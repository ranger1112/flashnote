# 安全策略

## 支持范围

只有 [Releases](https://github.com/ranger1112/flashnote/releases/latest) 中的最新版本会收到安全修复。旧版本请先升级再报告。

## 报告漏洞

请**不要**用公开 Issue 报告安全问题。通过 GitHub 的私下渠道提交：

1. 打开仓库的 **Security** 标签页；
2. 点击 **Report a vulnerability**（GitHub Private Vulnerability Reporting）。

请附上：复现步骤、影响范围（例如「任意本地文件读取」「窗口标题泄漏到笔记」）、闪记版本与 Windows 版本。我通常会在 72 小时内回复；确认问题后会在修复版本发布时致谢（如你希望匿名，请注明）。

## 设计边界

了解以下事实有助于判断问题是否属于漏洞：

- **纯本地**：闪记没有账号体系，也不向任何自建服务器上传数据；设置、草稿与日志都在本机。唯一的网络请求是检查和下载 GitHub Releases 上的更新（手动点击或按设置自动检查时）。
- **数据位置**：笔记写入设置里的笔记目录（默认 `文档\FlashNote`）；设置与草稿在 `%APPDATA%\com.phoebe.flashnote`；日志在 `%LOCALAPPDATA%\com.phoebe.flashnote\logs`，不记录笔记正文。
- **前端隔离**：WebView 里不直接访问文件系统，所有读写都经 Tauri command；历史预览渲染 Markdown 前用 DOMPurify 清洗，CSP 禁止内联脚本与远程资源。
- **安装包未签名**：首次运行可能触发 SmartScreen 提示，这属于已知情况，不是漏洞。校验请以 Releases 页面提供的文件为准。

## 已知的非安全问题

- 全局快捷键可能被其他程序占用，需要在设置里更换；
- 抓取前台窗口标题作为来源，可能包含你正在浏览的窗口名（可在设置中关闭）。
