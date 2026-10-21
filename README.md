# 闪记 FlashNote

不打断阅读的桌面速记小窗。看文章、看视频时按一下快捷键，写下临时想法，继续看。

## 使用

| 操作 | 说明 |
| --- | --- |
| `Ctrl+Alt+N` | 全局唤起 / 收起（可在设置里改） |
| `Ctrl+Enter` 或 `Ctrl+S` | 保存 |
| `Esc` | 收起，未保存内容保留为草稿 |
| 📌 | 常驻模式：保存后不关闭，窗口始终置顶 |
| 拖动标题栏 | 移动窗口，位置自动记住 |
| 右下角 | 拖动调整窗口大小 |

托盘图标：左键打开速记；右键菜单可打开今日笔记、笔记文件夹、设置或退出。

## 数据

默认保存到系统「文档」目录下的 `FlashNote\YYYY-MM-DD.md`（本机文档目录被重定向，实际为 `D:\Users\Phoebe\FlashNote`），可在设置里更换目录。
每条速记包含时间，以及（可选的）来源窗口标题：

```markdown
# 2026-10-21

- 14:32 这里是一个想法
  第二行会缩进两格
  _来源：Google Chrome — 某篇文章_
```

## 开发

```bash
npm install
npm run tauri dev
```

构建安装包：

```bash
npm run tauri build
```

本机已把 Rust 装在 `D:\dev\rust`、Build Tools 装在 `D:\BuildTools`，直接用封装脚本即可（会自动加载 MSVC 环境）：

```bat
scripts\dev.cmd
scripts\build.cmd
```

### 环境要求

- Node.js 20+
- Rust stable（MSVC 工具链）
- Visual Studio Build Tools 2022（Desktop development with C++）
- WebView2（Windows 10 1803+ 自带）
