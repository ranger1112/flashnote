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
  listen: vi.fn(async () => () => {}),
  isEnabled: vi.fn(async () => false),
  enable: vi.fn(async () => {}),
  disable: vi.fn(async () => {}),
  openDialog: vi.fn(async () => null),
  check: vi.fn(async () => null),
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
  historyPreview: false,
  autoUpdate: false,
  onboarded: true,
  theme: "dark",
  autoHideOnBlur: false,
  backgroundOpacity: 1,
  window: null,
};

function entry(time: string, content: string): NoteEntry {
  return {
    date: "2026-10-21",
    time,
    content,
    source: null,
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
    update_note_entry: () => null,
    open_note_file: () => null,
    open_notes_dir: () => null,
    complete_onboarding: () => null,
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
  vi.resetModules();
  document.documentElement.innerHTML = `<head></head><body>${BODY_HTML}</body>`;
  window.localStorage.clear();
  prepare?.();

  const routes = { ...defaultHandlers(), ...handlers };
  mocks.invoke.mockReset();
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

beforeEach(() => {
  vi.useRealTimers();
  notes = [entry("09:00", "第一条"), entry("08:00", "第二条")];
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
