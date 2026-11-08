// @vitest-environment happy-dom
import { beforeEach, describe, expect, it, vi } from "vitest";
import page from "../index.html?raw";
import type { NoteEntry } from "./lib";

/**
 * main.ts 是入口模块，导入即初始化并绑定 DOM。
 * 这里用 happy-dom 装载真实的 index.html，再把 Tauri IPC 全部换成可控的 mock，
 * 覆盖纯函数测试够不到的状态机（删除撤销、搜索竞态、草稿加载等）。
 */

const mocks = vi.hoisted(() => ({
  invoke: vi.fn<(cmd: string, args?: Record<string, unknown>) => Promise<unknown>>(),
  listen: vi.fn(async (_event: string, _handler: (event: unknown) => void) => () => {}),
  isEnabled: vi.fn(async () => false),
  enable: vi.fn(async () => {}),
  disable: vi.fn(async () => {}),
  openDialog: vi.fn(async () => null),
  check: vi.fn<() => Promise<unknown>>(async () => null),
  relaunch: vi.fn(async () => {}),
  isVisible: vi.fn(async () => false),
  onFocusChanged: vi.fn(async () => () => {}),
  startResizeDragging: vi.fn(async () => {}),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mocks.listen }));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    onFocusChanged: mocks.onFocusChanged,
    isVisible: mocks.isVisible,
    startResizeDragging: mocks.startResizeDragging,
  }),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: mocks.openDialog }));
vi.mock("@tauri-apps/plugin-autostart", () => ({
  isEnabled: mocks.isEnabled,
  enable: mocks.enable,
  disable: mocks.disable,
}));
vi.mock("@tauri-apps/plugin-updater", () => ({ check: mocks.check }));
vi.mock("@tauri-apps/plugin-process", () => ({ relaunch: mocks.relaunch }));

const DRAFT_KEY = "flashnote.draft";

// 直接复用真实页面结构，避免测试里手抄一份 DOM 快照
const body = /<body[^>]*>([\s\S]*)<\/body>/i.exec(page);
if (!body) throw new Error("index.html 里没有 <body>");
const BODY_HTML = body[1].replace(/<script[\s\S]*?<\/script>/gi, "");

const baseSettings = {
  notesDir: "D:\\Notes",
  hotkey: "ctrl+alt+n",
  pinned: false,
  captureSource: true,
  markdownMode: false,
  gitBackup: false,
  historyPreview: false,
  autoUpdate: false,
  onboarded: true,
  theme: "dark",
  autoHideOnBlur: false,
  backgroundOpacity: 1,
  window: null,
  sync: {
    kind: "webdav",
    enabled: false,
    url: "",
    username: "",
    intervalMinutes: 5,
    allowInsecureHttp: false,
    deviceName: "",
  },
};

interface TestSyncStatus {
  configured: boolean;
  enabled: boolean;
  running: boolean;
  lastSyncAt: string | null;
  lastError: string | null;
  degraded: string | null;
  summary: unknown;
  conflicts: number;
}

function syncStatus(overrides: Partial<TestSyncStatus> = {}): TestSyncStatus {
  return {
    configured: false,
    enabled: false,
    running: false,
    lastSyncAt: null,
    lastError: null,
    degraded: null,
    summary: null,
    conflicts: 0,
    ...overrides,
  };
}

function entry(time: string, content: string, tags: string[] = []): NoteEntry {
  return {
    date: "2026-10-21",
    time,
    content,
    source: null,
    tags,
    path: "D:\\Notes\\2026-10-21.md",
  };
}

let notes: NoteEntry[] = [];

type Handler = (args?: Record<string, unknown>) => unknown;
type Handlers = Record<string, Handler>;

function defaultHandlers(): Handlers {
  return {
    get_settings: () => ({ ...baseSettings }),
    load_draft: () => "",
    app_version: () => "0.0.0-test",
    take_startup_notice: () => null,
    list_notes: () => notes,
    search_notes: () => notes,
    set_pinned: () => null,
    save_note: () => ({ path: "D:\\Notes\\2026-10-21.md", time: "10:00" }),
    save_draft: () => null,
    clear_draft: () => null,
    hide_window: () => null,
    delete_note_entry: () => null,
    update_note_entry: () => [],
    open_note_file: () => null,
    open_notes_dir: () => null,
    complete_onboarding: () => null,
    get_sync_status: () => syncStatus(),
    get_sync_file_states: () => [],
    get_git_status: () => ({ available: true, repo: false }),
    init_notes_git: () => ({ available: true, repo: true }),
    update_git_backup: () => null,
    update_sync_config: () => syncStatus({ configured: true, enabled: true }),
    save_sync_password: () => ({ persistent: true, warning: null }),
    clear_sync_password: () => null,
    test_sync_connection: () => ({ files: 0, degraded: null, message: "连接成功" }),
    sync_now: () => null,
  };
}

/** 让排队的微任务跑完：main.ts 的初始化全是已 resolve 的 IPC Promise。 */
async function flush(times = 20): Promise<void> {
  for (let i = 0; i < times; i += 1) await Promise.resolve();
}

async function boot(
  handlers: Handlers = {},
  prepare?: () => void,
): Promise<void> {
  // 上一轮注册在 window/document 上的监听必须先摘掉，否则旧实例会响应新用例的事件。
  (
    globalThis as unknown as { __flashnoteTeardown?: () => void }
  ).__flashnoteTeardown?.();
  vi.resetModules();
  document.documentElement.innerHTML = `<head></head><body>${BODY_HTML}</body>`;
  window.localStorage.clear();
  prepare?.();

  const routes = { ...defaultHandlers(), ...handlers };
  mocks.invoke.mockReset();
  // 每个用例的模块实例都绑定到新的 DOM，旧的监听回调不能留在调用记录里
  mocks.listen.mockClear();
  mocks.invoke.mockImplementation(async (cmd, args) => {
    const handler = routes[cmd];
    if (!handler) throw new Error(`unexpected invoke: ${cmd}`);
    return handler(args);
  });

  await import("./main");
  await flush();
}

function node<T extends Element>(selector: string): T {
  const found = document.querySelector<T>(selector);
  if (!found) throw new Error(`missing ${selector}`);
  return found;
}

function entries(): Element[] {
  return Array.from(document.querySelectorAll("#entry-list .entry"));
}

async function openHistory(): Promise<void> {
  node<HTMLButtonElement>("#notes-btn").click();
  await flush();
}

function deleteButton(index = 0): HTMLButtonElement {
  const button = entries()[index]?.querySelector<HTMLButtonElement>(".entry-action.danger");
  if (!button) throw new Error("没有删除按钮");
  return button;
}

function editButton(index = 0): HTMLButtonElement {
  const button = entries()[index]?.querySelector<HTMLButtonElement>(".entry-action");
  if (!button) throw new Error("没有编辑按钮");
  return button;
}

beforeEach(() => {
  vi.useRealTimers();
  notes = [entry("09:00", "第一条"), entry("08:00", "第二条")];
});

describe("编辑保护", () => {
  it("编辑框打开时关闭面板会提示先保存，不丢输入", async () => {
    await boot();
    await openHistory();

    editButton().click();
    await flush();
    expect(document.querySelector(".entry-edit")).not.toBeNull();

    node<HTMLButtonElement>("#history-close").click();
    await flush();

    expect(node<HTMLElement>("#history").classList.contains("hidden")).toBe(false);
    expect(node<HTMLElement>("#toast-message").textContent).toContain("请先保存或取消编辑");

    // 再次点「笔记」（例如托盘唤起后）也不能重建列表、丢掉编辑器。
    const editorValue = node<HTMLTextAreaElement>(".entry-edit").value;
    node<HTMLButtonElement>("#notes-btn").click();
    await flush();
    const reopened = document.querySelector<HTMLTextAreaElement>(".entry-edit");
    expect(reopened?.value).toBe(editorValue);
  });

  it("保存期间继续输入时保留编辑器，不丢掉新文字", async () => {
    notes = [entry("09:00", "旧内容")];
    let finish: ((tags: string[]) => void) | undefined;
    await boot({
      update_note_entry: () =>
        new Promise<string[]>((resolve) => {
          finish = resolve;
        }),
    });
    await openHistory();

    editButton().click();
    await flush();
    const editor = node<HTMLTextAreaElement>(".entry-edit");
    editor.value = "第一版";
    node<HTMLButtonElement>(".entry-edit-actions .btn.primary").click();
    await flush();

    // 保存还没返回时再按一次 Ctrl+Enter 不会重复提交
    editor.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Enter", ctrlKey: true, bubbles: true }),
    );
    await flush();
    expect(
      mocks.invoke.mock.calls.filter(([cmd]) => cmd === "update_note_entry"),
    ).toHaveLength(1);

    // 等待期间继续输入；保存返回后编辑器必须保持打开。
    editor.value = "第一版 + 新输入";
    finish?.([]);
    await flush();

    expect(node<HTMLTextAreaElement>(".entry-edit").value).toBe("第一版 + 新输入");
  });

  it("撤销条初始不可聚焦", async () => {
    await boot();
    expect(node<HTMLElement>("#undo-bar").hasAttribute("inert")).toBe(true);
  });
});

describe("删除与撤销", () => {
  it("删除后 5 秒内撤销会恢复条目，且不会写盘", async () => {
    await boot();
    await openHistory();
    expect(entries()).toHaveLength(2);

    deleteButton().click();
    await flush();
    expect(entries()).toHaveLength(1);
    expect(node<HTMLElement>("#undo-bar").classList.contains("show")).toBe(true);

    node<HTMLButtonElement>("#undo-action").click();
    await flush();
    expect(entries()).toHaveLength(2);
    expect(mocks.invoke).not.toHaveBeenCalledWith("delete_note_entry", expect.anything());
  });

  it("撤销窗口到期后真正落盘", async () => {
    await boot();
    await openHistory();

    vi.useFakeTimers();
    deleteButton().click();
    await flush();
    await vi.advanceTimersByTimeAsync(5000);

    expect(mocks.invoke).toHaveBeenCalledWith(
      "delete_note_entry",
      expect.objectContaining({ time: "09:00" }),
    );
    vi.useRealTimers();
  });

  it("待删除条目不会因列表刷新而复活", async () => {
    await boot();
    await openHistory();

    deleteButton().click();
    await flush();
    // 服务端仍会返回这条笔记，刷新时应该被过滤掉
    await openHistory();

    expect(entries()).toHaveLength(1);
    expect(entries()[0].textContent).toContain("第二条");
  });
});

describe("搜索", () => {
  it("请求乱序返回时只采用最后一次结果", async () => {
    let resolveFirst!: (value: NoteEntry[]) => void;
    let resolveSecond!: (value: NoteEntry[]) => void;
    const first = new Promise<NoteEntry[]>((resolve) => {
      resolveFirst = resolve;
    });
    const second = new Promise<NoteEntry[]>((resolve) => {
      resolveSecond = resolve;
    });
    let calls = 0;

    await boot({
      search_notes: () => {
        calls += 1;
        return calls === 1 ? first : second;
      },
    });
    await openHistory();

    vi.useFakeTimers();
    const input = node<HTMLInputElement>("#history-search");
    input.value = "alpha";
    input.dispatchEvent(new Event("input"));
    await vi.advanceTimersByTimeAsync(250);

    input.value = "beta";
    input.dispatchEvent(new Event("input"));
    await vi.advanceTimersByTimeAsync(250);

    // 新请求先返回，旧请求随后才到
    resolveSecond([entry("10:00", "beta 结果")]);
    await flush();
    resolveFirst([entry("11:00", "alpha 结果")]);
    await flush();

    expect(entries()).toHaveLength(1);
    expect(entries()[0].textContent).toContain("beta 结果");
    vi.useRealTimers();
  });

  it("Esc 关闭历史后不再执行已排队的搜索", async () => {
    await boot();
    await openHistory();

    vi.useFakeTimers();
    const input = node<HTMLInputElement>("#history-search");
    input.value = "abc";
    input.dispatchEvent(new Event("input"));
    input.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    await vi.advanceTimersByTimeAsync(1000);

    expect(mocks.invoke).not.toHaveBeenCalledWith("search_notes", expect.anything());
    vi.useRealTimers();
  });
});

describe("设置", () => {
  it("常驻开关落盘失败时回滚按钮状态", async () => {
    await boot({ set_pinned: () => Promise.reject("磁盘只读") });
    const pin = node<HTMLButtonElement>("#pin");
    expect(pin.classList.contains("active")).toBe(false);

    pin.click();
    await flush();

    expect(pin.classList.contains("active")).toBe(false);
    expect(node<HTMLElement>("#toast-message").textContent).toContain("磁盘只读");
  });

  it("面板打开时背景（含更新横幅）一起 inert", async () => {
    await boot();
    const banner = node<HTMLElement>("#update-banner");
    expect(banner.inert).toBe(false);

    node<HTMLButtonElement>("#settings-btn").click();
    await flush();

    expect(banner.inert).toBe(true);
  });
});

describe("更新", () => {
  beforeEach(() => {
    mocks.check.mockReset();
    mocks.check.mockResolvedValue(null);
    mocks.relaunch.mockReset();
  });

  it("检查到新版本后可在设置面板内直接更新并重启", async () => {
    await boot({ flush_sync_state: () => null });
    node<HTMLButtonElement>("#settings-btn").click();
    await flush();

    const inline = node<HTMLButtonElement>("#update-now");
    expect(inline.hidden).toBe(true);

    const downloadAndInstall = vi.fn(async (onEvent: (event: unknown) => void) => {
      onEvent({ event: "Started", data: { contentLength: 100 } });
      onEvent({ event: "Progress", data: { chunkLength: 100 } });
    });
    mocks.check.mockResolvedValueOnce({ version: "0.6.0", downloadAndInstall });

    node<HTMLButtonElement>("#check-update").click();
    await flush();

    expect(inline.hidden).toBe(false);
    expect(inline.textContent).toBe("立即更新");
    expect(node<HTMLElement>("#update-status").textContent).toBe("发现新版本 0.6.0");

    inline.click();
    await flush();

    expect(downloadAndInstall).toHaveBeenCalledTimes(1);
    expect(node<HTMLElement>("#update-status").textContent).toContain("已安装 0.6.0");
    expect(inline.textContent).toBe("重启");

    inline.click();
    await flush();

    expect(mocks.invoke).toHaveBeenCalledWith("flush_sync_state");
    expect(mocks.relaunch).toHaveBeenCalledTimes(1);
  });

  it("已是最新版本时不显示「立即更新」", async () => {
    await boot();
    node<HTMLButtonElement>("#settings-btn").click();
    await flush();

    node<HTMLButtonElement>("#check-update").click();
    await flush();

    expect(node<HTMLButtonElement>("#update-now").hidden).toBe(true);
    expect(node<HTMLElement>("#update-status").textContent).toBe("已是最新版本");
  });

  it("安装失败时按钮变成「重试」，再点一次重新下载", async () => {
    await boot();
    node<HTMLButtonElement>("#settings-btn").click();
    await flush();

    const downloadAndInstall = vi.fn(async () => {
      throw new Error("网络中断");
    });
    mocks.check.mockResolvedValueOnce({ version: "0.6.0", downloadAndInstall });

    node<HTMLButtonElement>("#check-update").click();
    await flush();

    const inline = node<HTMLButtonElement>("#update-now");
    inline.click();
    await flush();

    expect(inline.textContent).toBe("重试");
    expect(node<HTMLElement>("#update-status").textContent).toBe("更新失败");

    inline.click();
    await flush();

    expect(downloadAndInstall).toHaveBeenCalledTimes(2);
  });
});

describe("Git 自动提交", () => {
  async function openGitSettings(): Promise<void> {
    node<HTMLButtonElement>("#settings-btn").click();
    await flush();
  }

  it("未初始化仓库时提示并可一键初始化", async () => {
    let repo = false;
    await boot({
      get_git_status: () => ({ available: true, repo }),
      init_notes_git: () => {
        repo = true;
        return { available: true, repo: true };
      },
    });
    await openGitSettings();

    const init = node<HTMLButtonElement>("#git-init");
    expect(init.hidden).toBe(false);
    expect(node<HTMLElement>("#git-status").textContent).toContain("还不是 Git 仓库");

    init.click();
    await flush();

    expect(mocks.invoke).toHaveBeenCalledWith("init_notes_git");
    expect(node<HTMLButtonElement>("#git-init").hidden).toBe(true);
    expect(node<HTMLElement>("#git-status").textContent).toContain("已是 Git 仓库");
  });

  it("没有 git 命令时禁用开关并说明原因", async () => {
    await boot({ get_git_status: () => ({ available: false, repo: false }) });
    await openGitSettings();

    expect(node<HTMLInputElement>("#git-backup").disabled).toBe(true);
    expect(node<HTMLElement>("#git-status").textContent).toContain("没有找到 git");
    expect(node<HTMLButtonElement>("#git-init").hidden).toBe(true);
  });

  it("自动提交失败时设置页显示最近错误", async () => {
    await boot({
      get_git_status: () => ({
        available: true,
        repo: true,
        lastSuccess: "2026-11-04 10:00",
        lastError: "git commit 失败：index.lock 已存在",
      }),
    });
    await openGitSettings();

    const status = node<HTMLElement>("#git-status");
    expect(status.textContent).toContain("自动提交失败");
    expect(status.textContent).toContain("index.lock");
    expect(status.classList.contains("warn")).toBe(true);
  });

  it("自动提交成功后设置页显示上次提交时间", async () => {
    await boot({
      get_git_status: () => ({
        available: true,
        repo: true,
        lastSuccess: "2026-11-04 10:05",
      }),
    });
    await openGitSettings();

    const status = node<HTMLElement>("#git-status");
    expect(status.textContent).toContain("上次自动提交");
    expect(status.textContent).toContain("10:05");
    expect(status.classList.contains("warn")).toBe(false);
  });
});

describe("历史面板错误态", () => {
  it("读取失败时显示持久错误，重试成功后恢复列表", async () => {
    let failing = true;
    await boot({
      list_notes: () => (failing ? Promise.reject("目录不可读") : notes),
    });
    await openHistory();

    const error = document.querySelector(".history-error");
    expect(error).not.toBeNull();
    expect(error?.textContent).toContain("读取笔记失败");
    expect(error?.textContent).toContain("目录不可读");
    expect(entries()).toHaveLength(0);

    failing = false;
    error?.querySelector<HTMLButtonElement>("button")?.click();
    await flush();

    expect(document.querySelector(".history-error")).toBeNull();
    expect(entries()).toHaveLength(2);
  });
});

describe("云同步", () => {
  async function openSettings(): Promise<void> {
    node<HTMLButtonElement>("#settings-btn").click();
    await flush();
  }

  function statusHandler(): (event: { payload: TestSyncStatus }) => void {
    const call = mocks.listen.mock.calls.find(([name]) => name === "sync-status");
    if (!call) throw new Error("没有监听 sync-status");
    return call[1] as unknown as (event: { payload: TestSyncStatus }) => void;
  }

  function openSettingsEvent(): void {
    const call = mocks.listen.mock.calls.find(([name]) => name === "open-settings");
    if (!call) throw new Error("没有监听 open-settings");
    (call[1] as unknown as () => void)();
  }

  it("重新打开设置面板不会清空未保存的同步表单", async () => {
    await boot();
    await openSettings();

    node<HTMLInputElement>("#sync-url").value = "https://draft.example.com/";
    node<HTMLInputElement>("#sync-username").value = "draft-user";
    node<HTMLInputElement>("#sync-password").value = "draft-pass";

    // 托盘「设置…」会再次走 openSettings()，不能重填表单
    openSettingsEvent();
    await flush();

    expect(node<HTMLInputElement>("#sync-url").value).toBe("https://draft.example.com/");
    expect(node<HTMLInputElement>("#sync-username").value).toBe("draft-user");
    expect(node<HTMLInputElement>("#sync-password").value).toBe("draft-pass");
  });

  it("保存时提交表单，密码单独落盘并清空输入框", async () => {
    let saved: Record<string, unknown> | undefined;
    await boot({
      update_sync_config: (args) => {
        saved = args?.config as Record<string, unknown>;
        return syncStatus({ configured: true, enabled: true });
      },
    });
    await openSettings();

    node<HTMLInputElement>("#sync-url").value = "  https://dav.example.com/ ";
    node<HTMLInputElement>("#sync-username").value = " alice ";
    node<HTMLInputElement>("#sync-enabled").checked = true;
    node<HTMLSelectElement>("#sync-interval").value = "15";
    node<HTMLInputElement>("#sync-password").value = "secret";

    node<HTMLButtonElement>("#sync-save").click();
    await flush();

    expect(saved).toMatchObject({
      url: "https://dav.example.com/",
      username: "alice",
      enabled: true,
      intervalMinutes: 15,
    });
    expect(mocks.invoke).toHaveBeenCalledWith("save_sync_password", {
      password: "secret",
    });
    expect(node<HTMLInputElement>("#sync-password").value).toBe("");
    expect(node<HTMLElement>("#toast-message").textContent).toContain("已保存");
  });

  it("切换到 GitHub 时字段标签与提示跟着变", async () => {
    await boot();
    await openSettings();

    const kind = node<HTMLSelectElement>("#sync-kind");
    expect(node<HTMLElement>("#sync-url-label").textContent).toBe("WebDAV 地址");

    kind.value = "github";
    kind.dispatchEvent(new Event("change"));
    await flush();

    expect(node<HTMLElement>("#sync-url-label").textContent).toBe("仓库地址");
    expect(node<HTMLElement>("#sync-username-label").textContent).toContain("分支");
    expect(node<HTMLElement>("#sync-password-label").textContent).toBe("Personal Access Token");
    expect(node<HTMLInputElement>("#sync-url").placeholder).toContain("github.com");
    // 明文 http 只对 WebDAV 有意义
    expect(node<HTMLElement>("#sync-allow-http").closest("label")?.hidden).toBe(true);
  });

  it("保存 GitHub 配置时把类型一起提交", async () => {
    let saved: Record<string, unknown> | undefined;
    await boot({
      update_sync_config: (args) => {
        saved = args?.config as Record<string, unknown>;
        return syncStatus({ configured: true, enabled: true });
      },
    });
    await openSettings();

    const kind = node<HTMLSelectElement>("#sync-kind");
    kind.value = "github";
    kind.dispatchEvent(new Event("change"));
    node<HTMLInputElement>("#sync-url").value = "https://github.com/alice/notes";
    node<HTMLButtonElement>("#sync-save").click();
    await flush();

    expect(saved).toMatchObject({
      kind: "github",
      url: "https://github.com/alice/notes",
    });
  });

  it("保存失败时保留输入并显示后端错误", async () => {
    await boot({
      update_sync_config: () => Promise.reject("地址格式不正确"),
    });
    await openSettings();

    node<HTMLInputElement>("#sync-url").value = "http://dav.example.com/";
    node<HTMLButtonElement>("#sync-save").click();
    await flush();

    expect(node<HTMLElement>("#sync-error").textContent).toContain("地址格式不正确");
    expect(node<HTMLInputElement>("#sync-url").value).toBe("http://dav.example.com/");
  });

  it("密码未持久化时不提示设置已保存", async () => {
    await boot({
      update_sync_config: () => syncStatus({ configured: true, enabled: true }),
      // 后端在凭据管理器不可用时就是这条路径：Ok + persistent:false。
      save_sync_password: () => ({
        persistent: false,
        warning: "当前系统没有可用的凭据管理器，密码只在本次会话内有效",
      }),
    });
    await openSettings();

    node<HTMLInputElement>("#sync-url").value = "https://dav.example.com/";
    node<HTMLInputElement>("#sync-username").value = "alice";
    node<HTMLInputElement>("#sync-password").value = "secret";
    node<HTMLButtonElement>("#sync-save").click();
    await flush();

    expect(node<HTMLElement>("#sync-error").textContent).toContain("密码未能保存");
    expect(node<HTMLElement>("#toast-message").textContent).not.toContain(
      "云同步设置已保存",
    );
  });

  it("面板打开时 Ctrl+S 不会保存主输入框或收起窗口", async () => {
    await boot();
    await openSettings();

    node<HTMLInputElement>("#sync-url").dispatchEvent(
      new KeyboardEvent("keydown", { key: "s", ctrlKey: true, bubbles: true }),
    );
    await flush();

    const commands = mocks.invoke.mock.calls.map(([command]) => command);
    expect(commands).not.toContain("save_note");
    expect(commands).not.toContain("hide_window");
  });

  it("地址变化且密码留空时提示重新输入密码", async () => {
    await boot();
    await openSettings();

    node<HTMLInputElement>("#sync-url").value = "https://dav.example.com/dav/";
    node<HTMLInputElement>("#sync-username").value = "alice";
    node<HTMLButtonElement>("#sync-save").click();
    await flush();

    expect(node<HTMLElement>("#sync-error").textContent).toContain("重新填写密码");
    expect(node<HTMLElement>("#toast-message").textContent).toContain("重新填写密码");
  });

  it("只给地址补末尾斜杠不触发重新输入密码", async () => {
    await boot({
      get_settings: () => ({
        ...baseSettings,
        sync: {
          ...baseSettings.sync,
          url: "https://dav.example.com/dav",
          username: "alice",
        },
      }),
    });
    await openSettings();

    node<HTMLInputElement>("#sync-url").value = "https://dav.example.com/dav/";
    node<HTMLButtonElement>("#sync-save").click();
    await flush();

    expect(node<HTMLElement>("#sync-error").textContent).not.toContain("重新填写密码");
  });

  it("测试连接失败显示在错误行，按钮恢复可用", async () => {
    await boot({
      test_sync_connection: () => Promise.reject("认证失败：401"),
    });
    await openSettings();

    node<HTMLButtonElement>("#sync-test").click();
    await flush();

    expect(node<HTMLElement>("#sync-error").textContent).toContain("认证失败");
    expect(node<HTMLButtonElement>("#sync-test").disabled).toBe(false);
  });

  it("收到状态广播后更新文案与冲突入口", async () => {
    await boot();
    await openSettings();

    const handler = statusHandler();
    handler({
      payload: syncStatus({
        configured: true,
        enabled: true,
        lastSyncAt: "2026-10-23T09:00:00Z",
        conflicts: 2,
      }),
    });
    await flush();

    expect(node<HTMLElement>("#sync-status-text").textContent).toContain("上次同步");
    const conflicts = node<HTMLButtonElement>("#sync-conflicts");
    expect(conflicts.hidden).toBe(false);
    expect(conflicts.textContent).toContain("2 个冲突副本");

    conflicts.click();
    await flush();
    expect(mocks.invoke).toHaveBeenCalledWith("open_notes_dir");
  });

  it("历史面板底部显示云同步未启用", async () => {
    await boot();
    await openHistory();

    expect(node<HTMLElement>("#sync-foot-status").textContent).toBe("云同步未启用");
    expect(node<HTMLButtonElement>("#sync-conflicts").hidden).toBe(true);
  });

  it("保存后显示待同步，同步完成后显示已同步", async () => {
    await boot({
      get_settings: () => ({
        ...baseSettings,
        sync: {
          ...baseSettings.sync,
          enabled: true,
          url: "https://dav.example.com/",
          username: "alice",
        },
      }),
    });
    const handler = statusHandler();
    handler({ payload: syncStatus({ configured: true, enabled: true }) });
    await flush();

    const textarea = node<HTMLTextAreaElement>("#note");
    textarea.value = "测试同步状态";
    textarea.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Enter", ctrlKey: true, bubbles: true }),
    );
    await flush();

    expect(node<HTMLElement>("#sync-indicator").textContent).toBe("待同步");

    handler({
      payload: syncStatus({
        configured: true,
        enabled: true,
        lastSyncAt: "2026-10-25T10:00:00Z",
        summary: {
          uploaded: 1,
          downloaded: 0,
          merged: 0,
          deleted: 0,
          skipped: 0,
          conflicts: 0,
          aborted: false,
          errors: [],
          warnings: [],
          startedAt: new Date(Date.now() + 1000).toISOString(),
          finishedAt: new Date(Date.now() + 2000).toISOString(),
        },
      }),
    });
    await flush();

    expect(node<HTMLElement>("#sync-indicator").textContent).toBe("已同步");
  });
});

describe("标签", () => {
  it("正文标签渲染成可点击的筛选器", async () => {
    notes = [entry("09:00", "开会 #工作", ["工作"]), entry("08:00", "别的事")];
    await boot({
      search_notes: (args) => (String(args?.query) === "#工作" ? [notes[0]] : notes),
    });
    await openHistory();

    const chip = document.querySelector<HTMLButtonElement>(".entry-tag");
    expect(chip?.textContent).toBe("#工作");

    chip?.click();
    await flush();

    expect(node<HTMLInputElement>("#history-search").value).toBe("#工作");
    expect(entries()).toHaveLength(1);
    expect(entries()[0].textContent).toContain("开会");

    // 再点同一个标签取消筛选
    document.querySelector<HTMLButtonElement>(".entry-tag")?.click();
    await flush();
    expect(node<HTMLInputElement>("#history-search").value).toBe("");
  });

  it("编辑后标签跟着刷新，不用重开面板", async () => {
    notes = [entry("09:00", "开会 #工作", ["工作"])];
    await boot({ update_note_entry: () => ["会议"] });
    await openHistory();

    editButton().click();
    await flush();
    const editor = node<HTMLTextAreaElement>(".entry-edit");
    editor.value = "开会 #会议";
    node<HTMLButtonElement>(".entry-edit-actions .btn.primary").click();
    await flush();

    const chips = Array.from(document.querySelectorAll(".entry-tag")).map(
      (chip) => chip.textContent,
    );
    expect(chips).toEqual(["#会议"]);
  });

  it("没有标签的笔记不渲染标签行", async () => {
    await boot();
    await openHistory();
    expect(document.querySelector(".entry-tag")).toBeNull();
  });
});

describe("历史面板的文件同步状态", () => {
  it("日期行显示后端返回的状态", async () => {
    await boot({
      get_sync_file_states: (args) =>
        (args?.dates as string[]).map((date) => ({ date, state: "pending" })),
    });
    await openHistory();

    const badge = document.querySelector<HTMLElement>(".entry-date .date-sync");
    expect(badge?.textContent).toBe("待同步");
    expect(badge?.className).toContain("pending");
    // 只请求当前列表里出现过的日期
    expect(mocks.invoke).toHaveBeenCalledWith("get_sync_file_states", {
      dates: ["2026-10-21"],
    });
  });

  it("同步状态接口失败时不影响列表", async () => {
    await boot({
      get_sync_file_states: () => Promise.reject("状态文件损坏"),
    });
    await openHistory();

    expect(entries()).toHaveLength(2);
    expect(document.querySelector(".entry-date .date-sync")?.textContent).toBe("");
  });
});

describe("草稿", () => {
  it("草稿读取返回时不覆盖用户已经输入的内容", async () => {
    let resolveDraft!: (value: string) => void;
    const draft = new Promise<string>((resolve) => {
      resolveDraft = resolve;
    });

    await boot({ load_draft: () => draft });
    const textarea = node<HTMLTextAreaElement>("#note");
    textarea.value = "用户已经输入";

    resolveDraft("旧草稿");
    await flush();

    expect(textarea.value).toBe("用户已经输入");
  });

  it("localStorage 草稿迁移失败时保留旧副本", async () => {
    await boot(
      { save_draft: () => Promise.reject("磁盘满") },
      () => window.localStorage.setItem(DRAFT_KEY, "旧草稿"),
    );

    expect(node<HTMLTextAreaElement>("#note").value).toBe("旧草稿");
    expect(window.localStorage.getItem(DRAFT_KEY)).toBe("旧草稿");
  });

  it("迁移成功后删除 localStorage 副本", async () => {
    await boot({}, () => window.localStorage.setItem(DRAFT_KEY, "旧草稿"));

    expect(window.localStorage.getItem(DRAFT_KEY)).toBeNull();
  });
});

describe("预览安全", () => {
  it("外链点击被拦截并交给系统浏览器", async () => {
    await boot();
    await openHistory();

    const content = node<HTMLElement>("#entry-list .entry-content");
    const anchor = document.createElement("a");
    anchor.setAttribute("href", "https://example.com/path");
    anchor.textContent = "外链";
    content.append(anchor);

    const event = new MouseEvent("click", { bubbles: true, cancelable: true });
    anchor.dispatchEvent(event);
    await flush();

    expect(event.defaultPrevented).toBe(true);
    expect(mocks.invoke).toHaveBeenCalledWith("open_external_url", {
      url: "https://example.com/path",
    });
  });

  it("图片映射区域的外链也会被拦截", async () => {
    await boot();
    await openHistory();

    const content = node<HTMLElement>("#entry-list .entry-content");
    const area = document.createElement("area");
    area.setAttribute("href", "https://evil.example/area");
    content.append(area);

    const event = new MouseEvent("click", { bubbles: true, cancelable: true });
    area.dispatchEvent(event);
    await flush();

    expect(event.defaultPrevented).toBe(true);
    expect(mocks.invoke).toHaveBeenCalledWith("open_external_url", {
      url: "https://evil.example/area",
    });
  });
});
