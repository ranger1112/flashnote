// @vitest-environment jsdom
import { describe, expect, it } from "vitest";
import { markdownHtml } from "./markdown";

/**
 * 预览 sanitizer 必须在真实 DOM 语义下验证：
 * happy-dom 下 DOMPurify 的 FORBID_TAGS/FORBID_ATTR 不生效，
 * 所以这个文件单独跑 jsdom。
 */
describe("markdownHtml", () => {
  it("剥离 SVG、area/map、表单控件与 class/id", () => {
    const html = markdownHtml(
      [
        "[外链](https://example.com/path)",
        '<map name="m"><area href="https://evil.example" shape="rect" coords="0,0,10,10"></map>',
        '<svg xmlns="http://www.w3.org/2000/svg"><a href="https://evil.example">svg</a></svg>',
        '<div class="settings" id="settings">fake</div>',
        '<form action="https://evil.example"><input name="password" type="password"></form>',
      ].join("\n"),
    );

    expect(html).not.toContain("<area");
    expect(html).not.toContain("<map");
    expect(html).not.toContain("<svg");
    expect(html).not.toContain("<form");
    expect(html).not.toContain("<input");
    expect(html).not.toContain('class="settings"');
    expect(html).not.toContain('id="settings"');
    // 正常外链保留下来，由点击拦截交给系统浏览器
    expect(html).toContain('href="https://example.com/path"');
  });

  it("保留常规 Markdown 结构", () => {
    const html = markdownHtml("## 标题\n\n- 一\n- 二\n\n```js\nconst a = 1;\n```");

    expect(html).toContain("<h2");
    expect(html).toContain("<ul");
    expect(html).toContain("<li");
    expect(html).toContain("<code");
  });

  it("不允许 javascript: 链接", () => {
    const html = markdownHtml("[x](javascript:alert(1))");
    expect(html.toLowerCase()).not.toContain("javascript:");
  });
});
