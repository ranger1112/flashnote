import { indentLess, indentMore } from "@codemirror/commands";
import { syntaxTree } from "@codemirror/language";
import type { EditorState, StateCommand } from "@codemirror/state";

/**
 * 选区覆盖的每个非空行都位于列表项内时返回 true。
 * 只有这种情况才让 Tab 缩进列表，其余位置仍交给浏览器切换焦点。
 */
function selectionAllInListItems(state: EditorState): boolean {
  for (const range of state.selection.ranges) {
    const first = state.doc.lineAt(range.from).number;
    const last = state.doc.lineAt(range.to).number;
    let sawContent = false;
    for (let number = first; number <= last; number++) {
      const line = state.doc.line(number);
      if (!line.text.trim()) continue;
      sawContent = true;
      if (!lineInListItem(state, line.from)) return false;
    }
    // 空文档或只有空白行时不算“在列表里”，Tab 继续用于切换焦点
    if (!sawContent) return false;
  }
  return true;
}

function lineInListItem(state: EditorState, pos: number): boolean {
  let node = syntaxTree(state).resolveInner(pos, 1);
  while (node.parent) {
    // 代码块里的 Tab 不应该改变列表层级
    if (node.name === "FencedCode" || node.name === "CodeBlock") return false;
    if (node.name === "ListItem") return true;
    node = node.parent;
  }
  return false;
}

export const indentListMore: StateCommand = (target) =>
  selectionAllInListItems(target.state) ? indentMore(target) : false;

export const indentListLess: StateCommand = (target) =>
  selectionAllInListItems(target.state) ? indentLess(target) : false;
