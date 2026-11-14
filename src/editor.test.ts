// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { CodeMirrorEditor, getEditor, initEditor, toggleSourceMode } from "./editor";

/**
 * happy-dom 没有排版引擎，getBoundingClientRect 永远是 0，编辑器会退回 textarea。
 * 这里给元素一个固定尺寸，让 CodeMirror 的真实主路径也能在测试里跑起来。
 */
const originalRect = Element.prototype.getBoundingClientRect;

function shimLayout(): void {
  Element.prototype.getBoundingClientRect = function () {
    return {
      x: 0,
      y: 0,
      top: 0,
      left: 0,
      right: 100,
      bottom: 100,
      width: 100,
      height: 100,
      toJSON() {},
    } as DOMRect;
  };
}

function mountPoint(): HTMLElement {
  const mount = document.createElement("div");
  document.body.append(mount);
  return mount;
}

async function measure(): Promise<void> {
  // 让 CodeMirror 的 measure/update 周期跑完
  await new Promise((resolve) => window.setTimeout(resolve, 0));
}

afterEach(() => {
  getEditor()?.destroy();
  Element.prototype.getBoundingClientRect = originalRect;
  document.body.replaceChildren();
});

describe("CodeMirror 编辑器主路径", () => {
  beforeEach(shimLayout);

  it("有布局能力时挂载 CodeMirror，而不是退回 textarea", () => {
    const editor = initEditor({ mount: mountPoint(), onChange: () => {} });
    expect(editor).toBeInstanceOf(CodeMirrorEditor);
    expect(document.querySelector("textarea")).toBeNull();
  });

  it("setText 更新正文，caret=end/start 落到末尾/开头", () => {
    const editor = initEditor({ mount: mountPoint(), onChange: () => {} }) as CodeMirrorEditor;
    editor.setText("第一行\n第二行", { caret: "end" });
    expect(editor.getText()).toBe("第一行\n第二行");
    expect(editor.view.state.selection.main.head).toBe("第一行\n第二行".length);

    editor.setText("abc", { caret: "start" });
    expect(editor.view.state.selection.main.head).toBe(0);
  });

  it("程序化 setText 不触发 onChange，用户编辑才触发", () => {
    const onChange = vi.fn();
    const editor = initEditor({ mount: mountPoint(), onChange }) as CodeMirrorEditor;

    editor.setText("外部载入");
    expect(onChange).not.toHaveBeenCalled();

    editor.view.dispatch({ changes: { from: 0, insert: "新" } });
    expect(onChange).toHaveBeenCalledTimes(1);
  });

  it("跨行链接标题不会让编辑器抛 RangeError", async () => {
    const editor = initEditor({ mount: mountPoint(), onChange: () => {} }) as CodeMirrorEditor;
    // 光标在链接外，跨行的 LinkTitle 会走隐藏路径
    editor.setText("[x](https://y '标题\n第二行')\n正文", { caret: "end" });
    await expect(measure()).resolves.toBeUndefined();
    expect(editor.getText()).toContain("第二行");
  });

  it("实时渲染在主路径生效，源码开关整体关闭装饰", async () => {
    const editor = initEditor({ mount: mountPoint(), onChange: () => {} }) as CodeMirrorEditor;
    editor.setText("# 标题\n正文", { caret: "end" });
    await measure();
    expect(editor.view.dom.querySelector(".cm-md-h1")).not.toBeNull();

    expect(toggleSourceMode()).toBe(true);
    await measure();
    expect(editor.view.dom.querySelector(".cm-md-h1")).toBeNull();

    expect(toggleSourceMode()).toBe(false);
    await measure();
    expect(editor.view.dom.querySelector(".cm-md-h1")).not.toBeNull();
  });
});
