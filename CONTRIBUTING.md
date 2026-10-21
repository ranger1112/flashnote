# 参与贡献

欢迎提交 Issue 和 Pull Request。这个项目只做一件事：**不打断当前工作流的速记小窗**。功能提案请先说明它如何服务于这一点。

## 开发环境

- Windows 10 / 11
- Node.js 20+
- Rust stable（MSVC 工具链）+ Visual Studio Build Tools 2022 的「使用 C++ 的桌面开发」
- WebView2 运行时

```bash
npm install
npm run tauri dev
```

也可以使用封装脚本（自动加载 MSVC 环境，路径按本机安装位置修改 `scripts\cargo.cmd`）：

```bat
scripts\dev.cmd
```

## 提交前自检

CI 会在 Windows 上跑同样的检查，本地先跑一遍可以少一轮往返：

```bash
npm run build                          # 类型检查 + 前端构建
cd src-tauri
cargo fmt --all -- --check             # 格式
cargo clippy --all-targets -- -D warnings
cargo test
```

## 代码约定

- **前端**：原生 TypeScript，不引入前端框架。用 `el()` 取 DOM、`invoke()` 调后端，不直接在 WebView 里读写文件。
- **后端**：文件读写集中在 `notes.rs` / `settings.rs`，命令层只做校验与调度。写入一律「临时文件 + 重命名」。
- **测试**：涉及解析、格式化、文件重建的逻辑都要有单元测试，放在同一文件的 `mod tests` 里，不依赖真实用户目录。
- **UI 文案**：界面为简体中文，注释也用中文。
- **格式**：Rust 交给 `cargo fmt`；前端没有格式化工具链，请保持与现有文件一致（缩进 2 空格、双引号、语句带分号），类型检查由 `tsc --noEmit` 把关。

## 提交信息

沿用约定式提交前缀，描述用中文：

```
feat: 新增历史面板搜索
fix: 修复热键冲突时旧快捷键未恢复
docs: 补充数据格式说明
chore(ci): 缓存 Rust 构建产物
```

一个提交只做一件事；涉及 UI 的改动请附截图。

## 提交 PR

- 在 PR 描述里写清改动目的、验证方式（手动步骤或新增测试）；
- 如果是修 Bug，请先建对应 Issue 并在 PR 中引用；
- 不提交 `node_modules/`、`dist/`、`src-tauri/target/` 等生成目录。

## 数据与隐私

笔记、设置、日志都在本机，应用不联网。提交 Issue 或截图时请自行打码笔记正文和窗口标题。
