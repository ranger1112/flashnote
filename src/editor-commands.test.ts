import { describe, expect, it } from "vitest";
import { EditorSelection, EditorState, type StateCommand } from "@codemirror/state";
import { ensureSyntaxTree } from "@codemirror/language";
import {
  insertNewlineContinueMarkupCommand,
  markdown,
  markdownLanguage,
} from "@codemirror/lang-markdown";
import { indentListLess, indentListMore } from "./editor-commands";

function makeState(doc: string, cursor = doc.length): EditorState {
  const state = EditorState.create({
    doc,
    selection: EditorSelection.cursor(cursor),
    extensions: [markdown({ base: markdownLanguage })],
  });
  // Tab 守卫依赖完整的 ListItem 语法树
  ensureSyntaxTree(state, doc.length, 1000);
  return state;
}

function run(command: StateCommand, state: EditorState): { ok: boolean; state: EditorState } {
  let next = state;
  const ok = command({
    state,
    dispatch: (transaction) => {
      next = transaction.state;
    },
  });
  return { ok, state: next };
}

const enter = insertNewlineContinueMarkupCommand({ nonTightLists: false });

describe("Markdown 列表续行", () => {
  it("无序列表回车插入同样的标记", () => {
    const result = run(enter, makeState("- 项目"));
    expect(result.ok).toBe(true);
    expect(result.state.doc.toString()).toBe("- 项目\n- ");
  });

  it("有序列表回车自动递增编号", () => {
    const result = run(enter, makeState("1. 第一项"));
    expect(result.ok).toBe(true);
    expect(result.state.doc.toString()).toBe("1. 第一项\n2. ");
  });

  it("在有序列表中间回车插入新编号", () => {
    const result = run(enter, makeState("1. 一\n3. 三", 4));
    expect(result.ok).toBe(true);
    expect(result.state.doc.toString()).toBe("1. 一\n2. \n3. 三");
  });

  it("空列表项回车直接退出列表", () => {
    const result = run(enter, makeState("- 一\n- "));
    expect(result.ok).toBe(true);
    expect(result.state.doc.toString()).toBe("- 一\n");
  });

  it("引用块回车继续 > 标记", () => {
    const result = run(enter, makeState("> 引用"));
    expect(result.ok).toBe(true);
    expect(result.state.doc.toString()).toBe("> 引用\n> ");
  });

  it("任务列表回车生成未勾选任务", () => {
    const result = run(enter, makeState("- [x] 完成"));
    expect(result.ok).toBe(true);
    expect(result.state.doc.toString()).toBe("- [x] 完成\n- [ ] ");
  });

  it("普通段落回车不续写标记", () => {
    const result = run(enter, makeState("普通段落"));
    expect(result.ok).toBe(false);
  });
});

describe("列表内 Tab 缩进", () => {
  it("列表项内 Tab 缩进两格", () => {
    const result = run(indentListMore, makeState("- 项目"));
    expect(result.ok).toBe(true);
    expect(result.state.doc.toString()).toBe("  - 项目");
  });

  it("列表项内 Shift+Tab 反缩进", () => {
    const result = run(indentListLess, makeState("  - 项目"));
    expect(result.ok).toBe(true);
    expect(result.state.doc.toString()).toBe("- 项目");
  });

  it("普通段落里 Tab 返回 false，保留焦点切换", () => {
    const state = makeState("普通段落");
    const result = run(indentListMore, state);
    expect(result.ok).toBe(false);
    expect(result.state.doc.toString()).toBe("普通段落");
  });

  it("空编辑器里 Tab 返回 false，不吞掉焦点切换", () => {
    const result = run(indentListMore, makeState(""));
    expect(result.ok).toBe(false);
    expect(result.state.doc.toString()).toBe("");
  });

  it("只有空白行时 Tab 也不缩进", () => {
    const result = run(indentListMore, makeState("   "));
    expect(result.ok).toBe(false);
    expect(result.state.doc.toString()).toBe("   ");
  });

  it("列表内代码块里的 Tab 不缩进列表", () => {
    const doc = "- 项目\n  ```\n  code\n  ```";
    const result = run(indentListMore, makeState(doc, doc.indexOf("code")));
    expect(result.ok).toBe(false);
  });
});
