import { describe, expect, it } from "vitest";
import {
  accelFromEvent,
  clampOpacity,
  formatAccel,
  matchesTerm,
  nextOffset,
  normalizeTheme,
  resolveTheme,
  sameEntry,
  withoutEntry,
  type AccelEvent,
  type NoteEntry,
} from "./lib";

function key(partial: Partial<AccelEvent>): AccelEvent {
  return {
    code: "KeyA",
    key: "a",
    ctrlKey: false,
    altKey: false,
    shiftKey: false,
    metaKey: false,
    ...partial,
  };
}

describe("formatAccel", () => {
  it("把修饰键显示成首字母大写", () => {
    expect(formatAccel("ctrl+alt+n")).toBe("Ctrl + Alt + N");
    expect(formatAccel("super+shift+f5")).toBe("Win + Shift + f5");
  });

  it("兼容 control / meta 这类别名", () => {
    expect(formatAccel("control+meta+k")).toBe("Ctrl + Win + K");
  });

  it("未知按键原样保留", () => {
    expect(formatAccel("ctrl+space")).toBe("Ctrl + space");
  });
});

describe("normalizeTheme", () => {
  it("接受三档合法主题并忽略大小写与空白", () => {
    expect(normalizeTheme("system")).toBe("system");
    expect(normalizeTheme(" Light ")).toBe("light");
    expect(normalizeTheme("DARK")).toBe("dark");
  });

  it("非法值与缺失值回落跟随系统", () => {
    expect(normalizeTheme("neon")).toBe("system");
    expect(normalizeTheme("")).toBe("system");
    expect(normalizeTheme(null)).toBe("system");
    expect(normalizeTheme(1)).toBe("system");
  });
});

describe("resolveTheme", () => {
  it("系统模式按系统偏好解析", () => {
    expect(resolveTheme("system", true)).toBe("light");
    expect(resolveTheme("system", false)).toBe("dark");
  });

  it("显式主题不受系统偏好影响", () => {
    expect(resolveTheme("light", false)).toBe("light");
    expect(resolveTheme("dark", true)).toBe("dark");
  });
});

describe("clampOpacity", () => {
  it("夹到 60%–100%", () => {
    expect(clampOpacity(0.2)).toBe(0.6);
    expect(clampOpacity(1.4)).toBe(1);
    expect(clampOpacity(0.8)).toBe(0.8);
    expect(clampOpacity(0.6)).toBe(0.6);
    expect(clampOpacity(1)).toBe(1);
  });

  it("非有限值按不透明处理", () => {
    expect(clampOpacity(Number.NaN)).toBe(1);
    expect(clampOpacity(Number.POSITIVE_INFINITY)).toBe(1);
  });
});

describe("accelFromEvent", () => {
  it("没有修饰键时拒绝", () => {
    expect(accelFromEvent(key({}))).toBeNull();
  });

  it("只按修饰键本身时拒绝", () => {
    expect(accelFromEvent(key({ code: "ControlLeft", key: "Control", ctrlKey: true }))).toBeNull();
  });

  it("字母、数字与功能键都能识别", () => {
    expect(accelFromEvent(key({ code: "KeyN", key: "n", ctrlKey: true, altKey: true }))).toBe(
      "ctrl+alt+n",
    );
    expect(accelFromEvent(key({ code: "Digit1", key: "1", ctrlKey: true }))).toBe("ctrl+1");
    expect(accelFromEvent(key({ code: "F12", key: "F12", ctrlKey: true }))).toBe("ctrl+f12");
    expect(accelFromEvent(key({ code: "F24", key: "F24", metaKey: true }))).toBe("super+f24");
  });

  it("符号键使用 Tauri 的写法", () => {
    expect(accelFromEvent(key({ code: "Space", key: " ", ctrlKey: true }))).toBe("ctrl+space");
    expect(accelFromEvent(key({ code: "Backquote", key: "`", ctrlKey: true }))).toBe("ctrl+`");
    expect(accelFromEvent(key({ code: "Minus", key: "-", ctrlKey: true }))).toBe("ctrl+-");
    expect(accelFromEvent(key({ code: "Equal", key: "=", ctrlKey: true }))).toBe("ctrl+=");
  });

  it("多个修饰键按 ctrl/alt/shift/super 的顺序拼接", () => {
    expect(
      accelFromEvent(
        key({ code: "KeyK", key: "k", ctrlKey: true, altKey: true, shiftKey: true, metaKey: true }),
      ),
    ).toBe("ctrl+alt+shift+super+k");
  });

  it("不支持的按键返回 null", () => {
    expect(accelFromEvent(key({ code: "F25", key: "F25", ctrlKey: true }))).toBeNull();
  });
});

describe("matchesTerm", () => {
  const note = {
    date: "2026-10-21",
    time: "14:32",
    content: "Rust 所有权笔记",
    source: "Google Chrome — 文章",
  };

  it("匹配正文", () => {
    expect(matchesTerm(note, "所有权")).toBe(true);
  });

  it("匹配日期、时间与来源（调用方传入小写词）", () => {
    expect(matchesTerm(note, "2026-10-21")).toBe(true);
    expect(matchesTerm(note, "14:32")).toBe(true);
    expect(matchesTerm(note, "chrome")).toBe(true);
  });

  it("来源为空时不影响匹配", () => {
    expect(matchesTerm({ ...note, source: null }, "rust")).toBe(true);
    expect(matchesTerm({ ...note, source: null }, "chrome")).toBe(false);
  });

  it("不匹配时返回 false", () => {
    expect(matchesTerm(note, "不存在")).toBe(false);
  });
});

function note(overrides: Partial<NoteEntry> = {}): NoteEntry {
  return {
    date: "2026-10-22",
    time: "10:00",
    content: "记一笔",
    source: null,
    path: "2026-10-22.md",
    ...overrides,
  };
}

describe("sameEntry", () => {
  it("定位参数全一致时认为是同一条", () => {
    expect(sameEntry(note(), note())).toBe(true);
  });

  it("时间或正文不同即不是同一条", () => {
    expect(sameEntry(note(), note({ time: "10:01" }))).toBe(false);
    expect(sameEntry(note(), note({ content: "另一条" }))).toBe(false);
  });

  it("来源不影响身份判断（后端删除时也不看来源）", () => {
    expect(sameEntry(note(), note({ source: "https://example.com" }))).toBe(true);
  });
});

describe("withoutEntry", () => {
  it("剔除匹配的条目并保留其余顺序", () => {
    const list = [note({ time: "09:00" }), note({ time: "10:00" }), note({ time: "11:00" })];
    expect(withoutEntry(list, note({ time: "10:00" })).map((item) => item.time)).toEqual([
      "09:00",
      "11:00",
    ]);
  });

  it("没有匹配时原样返回内容，不修改入参", () => {
    const list = [note({ time: "09:00" })];
    expect(withoutEntry(list, note({ time: "10:00" }))).toEqual(list);
    expect(list).toHaveLength(1);
  });
});

describe("nextOffset", () => {
  it("重置时取本批行数", () => {
    expect(nextOffset(0, 100, true)).toBe(100);
    expect(nextOffset(500, 30, true)).toBe(30);
  });

  it("追加时累加，即使本批有被客户端过滤掉的行也不回退", () => {
    expect(nextOffset(100, 100, false)).toBe(200);
    expect(nextOffset(200, 35, false)).toBe(235);
  });
});
