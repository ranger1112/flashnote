import { describe, expect, it } from "vitest";
import { accelFromEvent, formatAccel, matchesTerm, type AccelEvent } from "./lib";

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
