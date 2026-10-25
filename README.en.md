# FlashNote

**English** | [中文](README.md)

> A desktop quick-note popover that keeps your train of thought intact while you read articles or watch videos.

Press the global hotkey and a small window floats above the current window; jot down the thought, press `Ctrl+Enter` to save, and the window tucks itself away with focus back on the article or player. Entries are appended by date to local Markdown files — no account, no cloud upload.

**Download**: [Latest release](https://github.com/ranger1112/flashnote/releases/latest)

| Popover (light) | Popover (dark) |
| --- | --- |
| ![Light theme popover](docs/screenshot-light.png) | ![Dark theme popover](docs/screenshot-dark.png) |
| ![Note history](docs/history.png) | ![Settings panel](docs/settings.png) |

## Features

- **Global summon**: `Ctrl+Alt+N` by default; call up the popover from any application and press it again to dismiss.
- **Borderless floating window**: always-on-top, hidden from the taskbar, freely draggable and resizable, with position and size remembered automatically.
- **Two save modes**: "save and hide" by default; click the pin button for persistent mode, where the window stays open after saving.
- **Automatic source capture**: grabs the foreground window title when summoned and writes it into the entry when saving (Windows; can be disabled in settings).
- **Drafts never lost**: unsaved content is preserved when you dismiss with `Esc` and survives an app restart; drafts live in the app's config directory, so clearing browser data won't lose them.
- **Local-first**: Markdown split into files by day, readable directly with Obsidian, VS Code, and similar tools.
- **Optional cloud sync**: sync your notes directory to your own WebDAV storage (Jianguoyun, Nextcloud, NAS, …); off by default, no network traffic until configured. Concurrent edits are merged entry by entry, with conflict copies when a merge is unsafe.
- **Markdown mode**: optional block-based writing so code blocks, tables, and headings render correctly from the very first line.
- **Note history**: the note button in the title bar opens a history panel with reverse-chronological browsing, full-text search (across all date files), and loading of older entries; you can toggle a rendered Markdown preview and edit or delete individual entries directly.
- **System tray**: left-click to summon; right-click to open today's notes, the notes folder, settings, or quit.
- **Single instance**: launching again only summons the existing window — no second tray icon or hotkey conflict.
- **Auto-update**: silently checks GitHub Releases on startup; when a new version appears, a banner shows at the bottom of the window — click it to download, verify the signature, and install, effective after restart.
- **Appearance**: theme selection (system / light / dark) and adjustable background opacity (60%–100%).
- **Other**: optional launch at login, auto-hide on focus loss, multi-monitor boundary correction.

## Usage

| Action | Description |
| --- | --- |
| `Ctrl+Alt+N` | Global summon / dismiss (changeable in settings) |
| `Ctrl+Enter` or `Ctrl+S` | Save the current content |
| `Esc` | Dismiss the window; if the settings panel is open, closes the panel first |
| Note icon | Opens the note history panel; supports search, load more, preview, edit, and delete |
| Pin button | Toggles persistent mode: the window stays open after saving |
| Drag the title bar | Moves the window; the position is remembered automatically |
| Bottom-right handle | Drag to resize (minimum 380×240) |

If the content is empty when saving, the window simply dismisses and nothing is written to the file. Outside persistent mode, the window hides automatically after a successful save and focus returns to the previous application.

On first run a guide appears automatically, explaining the summon hotkey, where notes are saved, and what the pin does; it won't show again after you click "Get started", and you can revisit this information from settings later.

### Tray menu

Left-clicking the tray icon summons the popover directly; the right-click menu contains:

- **Show Quick Note**: same as the global hotkey.
- **View Notes**: summons the window and opens the note history panel.
- **Open Today's Note**: opens today's Markdown file with the system default application (usually VS Code / Typora; the file is created automatically if it doesn't exist).
- **Open Notes Folder**: opens the current notes directory.
- **Sync Now**: runs one sync round immediately; opens settings when sync is not configured or disabled.
- **Settings…**: summons the window and opens the settings panel.
- **Open Logs Folder**: opens the log directory, for troubleshooting.
- **Quit**: exits the application and releases the global hotkey.

## Settings

| Option | Description |
| --- | --- |
| Summon hotkey | Click "Record" and press a key combination; it must include at least one modifier (Ctrl / Alt / Shift / Win). On conflict, an error is shown and the previous hotkey is restored automatically; if the default hotkey is already taken at startup, the app falls back to an alternate combination and shows a notice in the window. |
| Notes directory | Changed via a folder picker; the directory is created automatically if it doesn't exist. |
| Capture source window | When off, the source title is no longer captured or written. |
| Markdown mode | When on, each entry is written as `### 时间` + the raw content, so block-level syntax such as code blocks and tables renders correctly; off by default. |
| Launch at login | Adds a startup entry for the current user; off by default. |
| Check for updates automatically | Silently checks for new versions on startup and rechecks every 6 hours while running; on by default. Turning it off stops queued checks immediately; you can still trigger a check manually with "Check for updates". |
| Hide when focus is lost | Tucks the window away when you click another application; off by default. Ignored in pin mode and while a panel is open. |
| Theme | System, light, or dark; system mode follows Windows light/dark changes live. |
| Background opacity | 60%–100%, applied to the window and its panels alike; 100% (opaque) by default. |
| Cloud sync (WebDAV) | Enable switch, URL, account, password, sync interval, and device name; off by default. See "Cloud sync (WebDAV)" below. |

Settings are stored at:

```
%APPDATA%\com.phoebe.flashnote\settings.json
```

Unsaved drafts are stored in `draft.txt` in the same directory and cleared automatically after a note is saved successfully.

Writes use a temp-file-plus-rename approach to avoid leaving corrupted JSON behind after an abnormal exit. If the file is already corrupt (for example, due to a bad manual edit), startup first backs it up as `settings.json.corrupt-<timestamp>`, then restores defaults and shows a notice.

## Data format

By default, notes are saved to `FlashNote\YYYY-MM-DD.md` under the system Documents folder; the directory can be changed in settings. The file for the day is created on the first write:

```markdown
# 2026-10-21

- 14:32 这里是一个想法
  第二行会缩进两格，仍属于同一条
  _来源：Google Chrome — 某篇文章_

- 15:02 另一个问题
  _来源：PotPlayer — 视频文件名_
```

Rules:

- Each entry starts with `- HH:MM ` using local time; crossing midnight writes to a new file automatically.
- Multi-line content is indented two spaces from the second line onward, keeping it part of the same entry.
- The source line uses the format `  _来源：窗口标题_` and is written only when capture succeeds; newlines in the title are collapsed into spaces.
- New notes are always appended; editing or deleting in the history panel rewrites only that day's file, creating a `2026-10-21.md.bak-<timestamp>` backup first (at most 5 kept per date).
- Files are UTF-8 without BOM with `\n` line endings.
- Cloud sync only handles `YYYY-MM-DD.md` and `YYYY-MM-DD.conflict-*.md`; `.bak-*`, `.tmp-*`, drafts, and settings are never uploaded.

### History panel

- Loads 100 entries at a time by default; "Load more" at the bottom pages further back.
- Search scans every `YYYY-MM-DD.md` in the notes directory, case-insensitive, covering body text, time, and source title, returning at most 200 results.
- "Preview" renders the entry content as Markdown (sanitized with DOMPurify); click again to return to the raw text. The toggle state is remembered.
- "Edit" rewrites the entry in place while preserving the source line and the original format (compact list / Markdown block); "Delete" removes the entry immediately, with a 5-second undo in the toast.

### Markdown mode

When enabled, each entry becomes a "time heading + raw content" instead of a list prefix with continuation indentation, so code blocks, tables, and headings render correctly from the first line; the source is written as an HTML comment, invisible in reading view:

````markdown
# 2026-10-21

### 14:32
## 小标题

```js
const a = 1;
```

<!-- 来源：VS Code — notes.rs -->
````

This mode is off by default and can be toggled in settings. When off, the compact list format above is used; the two formats can coexist, and older entries are never rewritten.

## Cloud sync (WebDAV)

Off by default. Sync uses only your own WebDAV storage; notes are written as **plain-text Markdown** under:

```
<your URL>/flashnote/notes/YYYY-MM-DD.md
```

Nothing outside that prefix is touched, and settings, drafts, and `.bak-*` files are never uploaded.

### Setup (Jianguoyun example)

1. Sign in to Jianguoyun on the web → "Account info" → "Security" → "Add app password" to get a dedicated password;
2. Open FlashNote settings → "Cloud sync (WebDAV)";
3. Use `https://dav.jianguoyun.com/dav/` as the URL, your Jianguoyun login email as the username, and the **app password** (not your login password);
4. Click "Test connection", then "Save" and enable "Cloud sync".

Nextcloud, NAS WebDAV, [dufs](https://github.com/sigoden/dufs), and `rclone serve webdav` work the same way — enter the DAV root URL (the directory, not a file).

### Behavior

- Triggers: 5 seconds after save / delete / edit; first round 60 seconds after launch; then every 5 / 15 / 60 minutes; the tray's "Sync Now" at any time.
- Only one round runs at a time; triggers during a round queue a follow-up round.
- Local deletions propagate to the remote; remote deletions move the local copy into `%APPDATA%\com.phoebe.flashnote\sync\<hash>\trash\<timestamp>\`.
- Edits on both sides of the same day are merged entry by entry (time + content + source); when a merge is unsafe, the remote content is saved as `YYYY-MM-DD.conflict-<device>-<timestamp>.md` and the history panel shows "N conflict copies".
- Safety valve: a round that would delete more than 5 files and more than 30% of tracked files aborts; an unreadable notes directory or a truncated remote listing also disables all deletions for that round.
- Weak servers (no strong ETag, ignored conditional writes, unreliable listings) are detected automatically and degrade to upload/download only — deletions are never propagated, and the reason is shown in settings.
- Each round has a request budget; leftover files wait for the next round to avoid rate limits.

### Credentials and privacy

- The password lives only in Windows Credential Manager (service `flashnote-sync`); `settings.json` contains just the URL, username, and non-sensitive options, and the UI never displays a saved password.
- If Credential Manager is disabled by group policy you get a clear error; sync still works for the current session but the password must be re-entered after restart.
- **Synced content is plain text**: your WebDAV provider can read the notes. End-to-end encryption is planned for "FlashNote Cloud" in 0.6.0.
- Changing the URL or username removes the old password automatically, so credentials are never sent to a different server.

### Troubleshooting

- "Authentication failed": Jianguoyun requires an app password; also make sure the URL ends with `/dav/`.
- "Too many requests": the provider is rate-limiting; FlashNote backs off according to `Retry-After`. Increase the interval or use fewer clients.
- Conflict copies are never cleaned up automatically; review and merge or delete them manually.
- Sync state and failure reasons appear at the bottom of settings and the history panel.

Protocol details (decision table, merge rules, state file) are in [docs/sync-protocol.md](docs/sync-protocol.md) (in Chinese); the WebDAV mode does not use its account or encryption sections.

## Installation

Download from [Releases](https://github.com/ranger1112/flashnote/releases/latest):

| Version | File | Description |
| --- | --- | --- |
| Installer | `FlashNote_<version>_x64-setup.exe` | Double-click to install; per-user installation, no administrator rights required |
| Portable | `FlashNote_<version>_portable_x64.zip` | Unzip and run `flashnote.exe` directly; nothing is written to an install directory |

Each release ships with a `SHA256SUMS.txt` that you can use to verify the download:

```powershell
Get-FileHash .\FlashNote_0.5.0_x64-setup.exe -Algorithm SHA256
```

Compare the output with the corresponding line in `SHA256SUMS.txt`.

The installers are unsigned, so SmartScreen may warn on first run; choose "More info → Run anyway". The cost, trade-offs, and adoption path for code signing are documented in [docs/code-signing.md](docs/code-signing.md) (in Chinese).

Uninstall: Windows "Settings → Apps", or run `uninstall.exe` in the install directory.

An installer built from source is located at:

```
src-tauri\target\release\bundle\nsis\FlashNote_0.5.0_x64-setup.exe
```

## Development

### Requirements

- Windows 10 / 11
- Node.js 22.12+ or 24 LTS
- Rust stable (MSVC toolchain)
- Visual Studio Build Tools 2022 with the "Desktop development with C++" workload selected
- WebView2 Runtime (usually already included on Windows 10 1803+)

### Starting the development environment

```bash
npm install
npm run tauri dev
```

On Windows you can also use the wrapper scripts, which locate Rust and MSVC automatically (`vswhere` + common install paths) and load the environment:

```bat
scripts\dev.cmd
scripts\build.cmd
scripts\cargo.cmd test      :: equivalent to cargo test, but with the environment ready
```

If the toolchain is installed in a non-default location (for example `D:\BuildTools` or a custom `CARGO_HOME`), copy the example to `scripts\_env.local.cmd` and fill in the paths; the file is ignored by `.gitignore`:

```bat
copy scripts\_env.local.cmd.example scripts\_env.local.cmd
```

### Building the installer

```bash
npm run tauri build
```

Outputs:

- Executable: `src-tauri\target\release\flashnote.exe`
- NSIS installer: `src-tauri\target\release\bundle\nsis\FlashNote_0.5.0_x64-setup.exe`

### Testing

```bash
npm test                 # Frontend tests: pure helpers + DOM state machine (happy-dom)
cd src-tauri && cargo test
```

Rust unit tests cover entry formatting, multi-line indentation, source-title normalization, file append separation, pagination, search, file rebuilding after edit/delete, atomic settings writes and corruption recovery, path traversal validation for the notes directory, and multi-monitor geometry clamping with DPI scaling; the frontend uses Vitest for pure helpers (`accelFromEvent` / `formatAccel`, search matching) and loads the real `index.html` in happy-dom to cover delete/undo, search races, settings rollback, history error states, and draft loading. Both run in CI.

### Releasing

Keep all five version fields in sync (`package.json`, `package-lock.json`, `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock`, `src-tauri/tauri.conf.json`):

```bash
npm run bump -- 0.4.1     # write the new version (npm needs `--` to pass args)
npm run version:check     # verify consistency (also run in CI)
```

Then update the CHANGELOG and the installer filenames in the READMEs, and tag:

```bash
git tag v0.4.1
git push origin v0.4.1
```

The `.github/workflows/release.yml` workflow builds the NSIS installer and portable zip, generates `SHA256SUMS.txt`, and creates a **draft** Release; review it and publish from GitHub. If the version numbers don't match, the workflow fails immediately, preventing a tagged release whose installer version differs.

Auto-update relies on two repository Actions secrets: `TAURI_SIGNING_PRIVATE_KEY` (the full minisign private key) and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. The workflow uses them to generate `.sig` files and `latest.json`; builds made without them cannot pass update verification. Keep the private key backed up locally only — never commit it to the repository. After publishing, the `latest.json` URL is:

```
https://github.com/ranger1112/flashnote/releases/latest/download/latest.json
```

This URL only points to the latest **published** stable release; drafts and prereleases are not picked up by auto-update.

### Project structure

```
flashnote/
├─ index.html                  # DOM structure for the popover and settings panel
├─ src/
│  ├─ main.ts                  # Entry and orchestration: startup, saving, show/hide, resize, global keys
│  ├─ dom.ts                   # Cached DOM element references
│  ├─ panels.ts                # Panel visibility, focus trapping, background inert
│  ├─ history.ts               # Note history: list, search, paging, edit, delete/undo
│  ├─ settings-panel.ts        # Settings panel and hotkey recording
│  ├─ settings-store.ts        # In-memory settings state and loading
│  ├─ appearance.ts            # Theme and background opacity
│  ├─ draft.ts                 # Draft autosave and restore
│  ├─ sync.ts                  # Cloud sync settings, status subscription, conflict hint
│  ├─ updater.ts               # Update checks, install, notice banner
│  ├─ welcome.ts               # First-run guide
│  ├─ toast.ts                 # Toasts and undo bar
│  ├─ lib.ts                   # Pure helpers: hotkey parsing, search matching, paging offset
│  ├─ lib.test.ts              # Frontend pure-function tests (Vitest)
│  ├─ main.dom.test.ts         # Frontend state-machine DOM tests (happy-dom)
│  └─ styles.css               # Light/dark themes, floating card styles
├─ src-tauri/
│  ├─ src/
│  │  ├─ main.rs               # App entry: plugins, tray, single instance, hotkey registration
│  │  ├─ commands.rs           # Frontend/backend commands: show/hide, save, settings
│  │  ├─ files.rs              # Opening files/folders, URL allowlist, note rewriting (backup + atomic replace)
│  │  ├─ geometry.rs           # Window geometry: multi-monitor clamping, DPI scaling, restore
│  │  ├─ notes.rs              # Markdown entry formatting and per-day appending (with tests)
│  │  ├─ settings.rs           # Atomic settings.json read/write and defaults
│  │  ├─ source.rs             # Foreground window title capture (Win32)
│  │  ├─ sync/
│  │  │  ├─ mod.rs             # Sync engine: scan, decision table, conflict copies, safety valve
│  │  │  ├─ backend.rs         # Backend abstraction: versions, preconditions, errors, capabilities
│  │  │  ├─ webdav.rs          # WebDAV client, capability probe, XML parsing
│  │  │  ├─ state.rs           # state.json, base copies, trash
│  │  │  ├─ merge.rs           # Entry-level three-way merge (line-span splicing)
│  │  │  ├─ runtime.rs         # Scheduling, single-flight, status broadcast
│  │  │  └─ secrets.rs         # Password storage (Windows Credential Manager)
│  │  └─ tray.rs               # Tray icon and menu
│  ├─ capabilities/default.json
│  ├─ icons/
│  ├─ tauri.conf.json
│  └─ Cargo.toml
├─ scripts/                    # Dev/build wrapper scripts and debugging tools
└─ package.json
```

## Technical notes

- **Framework**: Tauri 2 + Rust 2021, with a frontend built on plain TypeScript + Vite and no frontend framework.
- **Plugins**: `single-instance` (single instance), `global-shortcut` (global hotkeys), `autostart` (launch at login), `dialog` (directory picker), `updater` / `process` (update check, install, and restart).
- **Frontend/backend boundary**: the frontend never touches the filesystem directly; saving, settings, and window visibility all go through Tauri commands.
- **Window**: borderless with a transparent background; 10px of padding on all sides leaves room for CSS-drawn rounded corners and shadow; `skipTaskbar` keeps it out of the taskbar.
- **Source capture**: `GetForegroundWindow` + `GetWindowTextW`, excluding FlashNote itself by PID; if capture fails, the previous source is reused.
- **Saving**: appends to a file per date and replaces the settings file atomically; file I/O and directory creation run on the blocking pool, and settings are flushed asynchronously with only the newest snapshot winning, so the UI thread never waits on disk.
- **Cloud sync**: `reqwest` (system certificates) plus `quick-xml` for PROPFIND; sync state is keyed by notes directory **and** remote identity, so switching servers never reuses old records; passwords are stored in Windows Credential Manager via `keyring`, never in plain text.
- **Logging**: written to `%LOCALAPPDATA%\com.phoebe.flashnote\logs`, rotating when a single file exceeds 2MB and keeping the 3 most recent files; openable directly from the tray menu. Logs never contain note bodies.

## Platform support

Currently verified only on **Windows 10/11**. Source-window capture relies on the Win32 API; other platforms omit the source line. macOS / Linux are untested.

## Known limitations

- No export yet; search scans files one by one, so it gets slower with very large note collections.
- Cloud sync supports WebDAV only, and content is stored as plain text in your own storage; there is no end-to-end encryption or real-time push.
- If another application has claimed the global hotkey, you need to change it in settings.
- Note files are append-only and never rotated, so a single day's file grows gradually over long-term use.
- The installers are not code-signed, so the first install triggers a SmartScreen warning; see [docs/code-signing.md](docs/code-signing.md) for the research and the adoption path.

## FAQ

**The hotkey doesn't respond?**

Most likely another application has claimed it. If registering the default hotkey fails at startup, FlashNote tries `Ctrl+Alt+M`, then `Ctrl+Shift+Alt+N`, and shows a notice in the window. You can also right-click the tray icon → Settings and click "Record" to pick another combination; the hotkey must include at least one of Ctrl / Alt / Shift / Win.

**Where are my notes stored?**

By default in `%USERPROFILE%\Documents\FlashNote`, one Markdown file per date; you can change the directory in settings, and the tray menu opens it directly. Drafts and settings live in `%APPDATA%\com.phoebe.flashnote`, and logs in `%LOCALAPPDATA%\com.phoebe.flashnote\logs`.

**How do I uninstall it?**

Uninstall "FlashNote" (闪记) from Windows "Settings → Apps", or run `uninstall.exe` in the install directory (`%LOCALAPPDATA%\FlashNote`). Your notes directory is not deleted; remove it manually if needed.

**Why does Windows say "Windows protected your PC"?**

The installer is not code-signed, and SmartScreen blocks new programs with few downloads: click "More info" → "Run anyway". You can also verify file integrity with `SHA256SUMS.txt` from the release.

**Will upgrading touch my notes?**

No. Auto-update only replaces program files; notes, settings, and drafts all live in your user directory and are untouched by upgrades or uninstalls.

**Does it still go online after I turn off "Check for updates automatically"?**

No proactive network access. Once disabled, it only contacts GitHub Releases when you click "Check for updates" manually. Cloud sync is off by default and makes no requests until configured.

**Where does cloud sync send my notes?**

Only to the `flashnote/notes/` directory under the WebDAV URL you enter, as plain-text Markdown. FlashNote has no cloud server of its own. To use a second computer, enter the same URL and account there and the notes are pulled down.

**Can two computers editing the same day lose notes?**

No whole-file overwrites. FlashNote merges both sides' additions and edits entry by entry; when a merge is unsafe it keeps your local version and saves the remote content as `YYYY-MM-DD.conflict-<device>-<timestamp>.md`. The "N conflict copies" button at the bottom of the history panel opens the folder.

## Contributing

Issues and pull requests are welcome; see [CONTRIBUTING.md](CONTRIBUTING.md) for the exact process. Before submitting, make sure that:

- `npm run build` (type check + frontend build) passes;
- `npm test` (frontend unit tests) passes;
- `cd src-tauri && cargo test` passes;
- generated directories such as `node_modules/`, `dist/`, and `src-tauri/target/` are not committed.

See [CHANGELOG.md](CHANGELOG.md) for version changes; report security issues privately as described in [SECURITY.md](SECURITY.md) instead of opening a public issue.

## License

This project is open source under the [MIT License](LICENSE).
