import { describe, expect, it } from "vitest";
import { EditorSelection, EditorState } from "@codemirror/state";
import { ensureSyntaxTree } from "@codemirror/language";
import { markdown, markdownLanguage } from "@codemirror/lang-markdown";
import { buildPreviewAtomicRanges, buildPreviewDecorations } from "./live-preview";

interface Item {
  from: number;
  to: number;
  spec: Record<string, unknown>;
}

function makeState(doc: string, cursor = doc.length): EditorState {
  const state = EditorState.create({
    doc,
    selection: EditorSelection.cursor(cursor),
    extensions: [markdown({ base: markdownLanguage })],
  });
  ensureSyntaxTree(state, doc.length, 1000);
  return state;
}

function decorations(doc: string, cursor = doc.length): Item[] {
  const state = makeState(doc, cursor);
  const items: Item[] = [];
  buildPreviewDecorations(state).between(0, state.doc.length, (from, to, value) => {
    items.push({ from, to, spec: value.spec as Record<string, unknown> });
  });
  return items;
}

function atomicRanges(doc: string, cursor = doc.length): Item[] {
  const state = makeState(doc, cursor);
  const items: Item[] = [];
  buildPreviewAtomicRanges(state).between(0, state.doc.length, (from, to, value) => {
    items.push({ from, to, spec: value.spec as Record<string, unknown> });
  });
  return items;
}

function hasClass(items: Item[], className: string): boolean {
  return items.some(
    (item) =>
      typeof item.spec.class === "string" &&
      (item.spec.class as string).split(" ").includes(className),
  );
}

function hasMarker(items: Item[], from: number, to: number): boolean {
  return items.some(
    (item) =>
      item.from === from &&
      item.to === to &&
      typeof item.spec.class === "string" &&
      (item.spec.class as string).includes("cm-md-marker"),
  );
}

function isHidden(items: Item[], from: number, to: number): boolean {
  return items.some(
    (item) =>
      item.from === from &&
      item.to === to &&
      item.spec.class === undefined &&
      item.spec.widget === undefined,
  );
}

describe("Live Preview 块级渲染", () => {
  it("光标离开标题时放大标题并隐藏 # 标记", () => {
    const items = decorations("# 标题\n正文");
    expect(hasClass(items, "cm-md-h1")).toBe(true);
    expect(isHidden(items, 0, 2)).toBe(true);
  });

  it("光标在标题行时显示原始 # 标记", () => {
    const items = decorations("# 标题\n正文", 3);
    expect(hasMarker(items, 0, 1)).toBe(true);
    expect(isHidden(items, 0, 2)).toBe(false);
  });

  it("闭合式标题同时隐藏首尾 # 标记", () => {
    const items = decorations("# 标题 #\n正文");
    expect(hasClass(items, "cm-md-h1")).toBe(true);
    // 起始标记连同后面的空格一起隐藏
    expect(isHidden(items, 0, 2)).toBe(true);
    expect(isHidden(items, 5, 6)).toBe(true);
  });

  it("无序列表标记在光标离开时变成圆点", () => {
    const items = decorations("- 项目\n正文");
    const bullet = items.find((item) => item.from === 0 && item.to === 1)?.spec.widget;
    expect(bullet?.constructor.name).toBe("BulletWidget");
    expect(hasClass(items, "cm-md-list-item")).toBe(true);
  });

  it("引用块隐藏 > 标记并加左侧样式", () => {
    const items = decorations("> 引用\n正文");
    expect(hasClass(items, "cm-md-quote")).toBe(true);
    expect(isHidden(items, 0, 2)).toBe(true);
  });

  it("代码块内容加等宽样式并隐藏围栏标记", () => {
    const items = decorations("```js\ncode\n```\n正文");
    expect(hasClass(items, "cm-md-code")).toBe(true);
    expect(isHidden(items, 0, 3)).toBe(true);
  });

  it("分割线在光标离开时替换成横线样式", () => {
    const items = decorations("---\n正文");
    expect(hasClass(items, "cm-md-hr")).toBe(true);
    expect(isHidden(items, 0, 3)).toBe(true);
  });

  it("缩进标题和引用中的标题都能识别块上下文", () => {
    const indented = decorations("  # 标题\n正文");
    expect(hasClass(indented, "cm-md-h1")).toBe(true);
    expect(isHidden(indented, 2, 4)).toBe(true);

    const quoted = decorations("> # 标题\n正文");
    expect(hasClass(quoted, "cm-md-quote")).toBe(true);
    expect(hasClass(quoted, "cm-md-h1")).toBe(true);
    expect(isHidden(quoted, 0, 2)).toBe(true);
    expect(isHidden(quoted, 2, 4)).toBe(true);
  });

  it("缩进分隔线和引用中的分隔线仍渲染为横线", () => {
    const indented = decorations("  ---\n正文");
    expect(hasClass(indented, "cm-md-hr")).toBe(true);
    expect(isHidden(indented, 2, 5)).toBe(true);

    const quoted = decorations("> ---\n正文");
    expect(hasClass(quoted, "cm-md-hr")).toBe(true);
    expect(hasClass(quoted, "cm-md-quote")).toBe(true);
    expect(isHidden(quoted, 0, 2)).toBe(true);
    expect(isHidden(quoted, 2, 5)).toBe(true);
  });

  it("只有标记的短行也能拿到块样式", () => {
    const emptyHeading = decorations("#\n正文");
    expect(hasClass(emptyHeading, "cm-md-h1")).toBe(true);
    expect(isHidden(emptyHeading, 0, 1)).toBe(true);

    const emptyQuote = decorations(">\n正文");
    expect(hasClass(emptyQuote, "cm-md-quote")).toBe(true);
    expect(isHidden(emptyQuote, 0, 1)).toBe(true);

    const emptyItem = decorations("-\n正文");
    expect(hasClass(emptyItem, "cm-md-list-item")).toBe(true);
    const bullet = emptyItem.find((item) => item.from === 0 && item.to === 1)?.spec.widget;
    expect(bullet?.constructor.name).toBe("BulletWidget");
  });

  it("列表或引用中的围栏代码能识别代码块", () => {
    const quoted = decorations("> ```js\n> code\n> ```\n正文");
    expect(hasClass(quoted, "cm-md-code")).toBe(true);
    expect(hasClass(quoted, "cm-md-quote")).toBe(true);
    expect(isHidden(quoted, 2, 5)).toBe(true);

    const inList = decorations("- ```js\n  code\n  ```\n正文");
    expect(hasClass(inList, "cm-md-code")).toBe(true);
    expect(hasClass(inList, "cm-md-list-item")).toBe(true);
  });

  it("缩进代码块也有等宽样式", () => {
    const items = decorations("    code\n正文");
    expect(hasClass(items, "cm-md-code")).toBe(true);
  });
});

describe("Live Preview 行内渲染", () => {
  it("加粗在光标离开时应用粗体并隐藏 **", () => {
    const items = decorations("**粗体**\n正文");
    expect(hasClass(items, "cm-md-strong")).toBe(true);
    expect(isHidden(items, 0, 2)).toBe(true);
    expect(isHidden(items, 4, 6)).toBe(true);
  });

  it("光标在加粗文字内时显示 **", () => {
    const items = decorations("**粗体**\n正文", 3);
    expect(hasMarker(items, 0, 2)).toBe(true);
    expect(isHidden(items, 0, 2)).toBe(false);
  });

  it("行内代码隐藏反引号并加等宽样式", () => {
    const items = decorations("`代码`\n正文");
    expect(hasClass(items, "cm-md-code-inline")).toBe(true);
    expect(isHidden(items, 0, 1)).toBe(true);
    expect(isHidden(items, 3, 4)).toBe(true);
  });

  it("链接只保留文字并隐藏括号与 URL", () => {
    const items = decorations("[文字](https://x)\n正文");
    expect(hasClass(items, "cm-md-link")).toBe(true);
    expect(isHidden(items, 0, 1)).toBe(true);
    expect(isHidden(items, 5, 14)).toBe(true);
  });

  it("自动链接保留 URL，只隐藏尖括号", () => {
    const items = decorations("<https://x>\n正文");
    expect(hasClass(items, "cm-md-link")).toBe(true);
    expect(isHidden(items, 0, 1)).toBe(true);
    expect(isHidden(items, 1, 10)).toBe(false);
  });

  it("任务列表标记保持原文", () => {
    const items = decorations("- [ ] 任务\n正文");
    expect(isHidden(items, 2, 5)).toBe(false);
  });
});

describe("Live Preview 原子范围", () => {
  it("隐藏的行内标记进入 atomicRanges，避免方向键卡住", () => {
    const ranges = atomicRanges("**粗体**\n正文");
    expect(ranges.some((item) => item.from === 0 && item.to === 2)).toBe(true);
  });
});
