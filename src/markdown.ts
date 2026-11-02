import DOMPurify from "dompurify";
import { marked } from "marked";

/**
 * 把笔记正文渲染成可安全插入预览的 HTML。
 *
 * 预览内容来自同步或粘贴，属于不可信数据：
 * - 只保留 HTML profile，去掉 SVG/MathML（SVG 链接可绕过点击拦截）
 * - 禁掉能导航的 area/map、能伪装应用 UI 的 class/id，以及表单控件
 */
export function markdownHtml(markdown: string): string {
  const html = marked.parse(markdown, { async: false });
  return DOMPurify.sanitize(String(html), {
    USE_PROFILES: { html: true },
    FORBID_TAGS: [
      "style",
      "svg",
      "math",
      "map",
      "area",
      "iframe",
      "object",
      "embed",
      "form",
      "input",
      "button",
      "select",
      "textarea",
    ],
    FORBID_ATTR: ["style", "usemap", "target", "class", "id"],
  });
}
