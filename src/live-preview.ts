import type { EditorState, Extension, Range } from "@codemirror/state";
import { StateEffect } from "@codemirror/state";
import { ensureSyntaxTree, syntaxTree } from "@codemirror/language";
import {
  Decoration,
  type DecorationSet,
  EditorView,
  ViewPlugin,
  WidgetType,
  type ViewUpdate,
} from "@codemirror/view";
import type { SyntaxNode } from "@lezer/common";

/** 组合输入结束、鼠标点击结束后强制重算装饰。 */
export const forcePreviewRebuild = StateEffect.define<null>();

const MARKER_CLASS = "cm-md-marker";
const MOUSE_REVEAL_DELAY_MS = 350;

const HEADING_NAMES = [
  "ATXHeading1",
  "ATXHeading2",
  "ATXHeading3",
  "ATXHeading4",
  "ATXHeading5",
  "ATXHeading6",
  "SetextHeading1",
  "SetextHeading2",
] as const;

const INLINE_MARKERS: Record<string, string[]> = {
  StrongEmphasis: ["EmphasisMark"],
  Emphasis: ["EmphasisMark"],
  Strikethrough: ["StrikethroughMark"],
  InlineCode: ["CodeMark"],
};

function headingLevel(name: string): number {
  const level = Number(name.slice(-1));
  return level >= 1 && level <= 6 ? level : 1;
}

/** 选区是否触及 [from, to]；边界也算触及，和 Obsidian 的“光标附近即显示源码”一致。 */
function touches(state: EditorState, from: number, to: number): boolean {
  return state.selection.ranges.some((range) => range.from <= to && range.to >= from);
}

interface LineContext {
  heading: SyntaxNode | null;
  blockquote: SyntaxNode | null;
  fence: SyntaxNode | null;
  codeBlock: SyntaxNode | null;
  rule: SyntaxNode | null;
  item: SyntaxNode | null;
}

const LINE_BLOCK_NAMES = new Set<string>([
  ...HEADING_NAMES,
  "Blockquote",
  "FencedCode",
  "CodeBlock",
  "HorizontalRule",
  "ListItem",
]);

/**
 * 按整行范围收集块级祖先。单点 resolveInner 在缩进、短行（# / > / ---）
 * 和嵌套块（> #、- ```）上会落到标记内部，拿不到块节点。
 */
function lineContext(
  tree: ReturnType<typeof syntaxTree>,
  lineFrom: number,
  lineTo: number,
): LineContext {
  const context: LineContext = {
    heading: null,
    blockquote: null,
    fence: null,
    codeBlock: null,
    rule: null,
    item: null,
  };
  tree.iterate({
    from: lineFrom,
    to: lineTo,
    enter: (ref) => {
      if (!LINE_BLOCK_NAMES.has(ref.name)) return undefined;
      switch (ref.name) {
        case "Blockquote":
          context.blockquote = ref.node;
          break;
        case "FencedCode":
          context.fence = ref.node;
          break;
        case "CodeBlock":
          context.codeBlock = ref.node;
          break;
        case "HorizontalRule":
          context.rule = ref.node;
          break;
        case "ListItem":
          context.item = ref.node;
          break;
        default:
          context.heading = ref.node;
      }
      return undefined;
    },
  });
  return context;
}

function hideOrMark(
  node: SyntaxNode,
  revealed: boolean,
  decorations: Range<Decoration>[],
  atomic: Range<Decoration>[],
  allowAtomic = true,
): void {
  if (revealed) {
    decorations.push(Decoration.mark({ class: MARKER_CLASS }).range(node.from, node.to));
    return;
  }
  decorations.push(Decoration.replace({}).range(node.from, node.to));
  if (allowAtomic) {
    atomic.push(Decoration.mark({}).range(node.from, node.to));
  }
}

function addLineDecorations(
  state: EditorState,
  tree: ReturnType<typeof syntaxTree>,
  lineFrom: number,
  lineTo: number,
  decorations: Range<Decoration>[],
  atomic: Range<Decoration>[],
): void {
  const line = state.doc.lineAt(lineFrom);
  const context = lineContext(tree, lineFrom, lineTo);
  const revealed = touches(state, lineFrom, lineTo);
  const classes: string[] = [];

  const heading = context.heading;
  if (heading) {
    // Setext 标题的正文行才放大，下划线行保持原样
    const textLine = state.doc.lineAt(heading.from);
    if (!heading.name.startsWith("Setext") || line.number === textLine.number) {
      classes.push(`cm-md-h${headingLevel(heading.name)}`);
    }
  }
  if (context.blockquote) classes.push("cm-md-quote");
  if (context.fence || context.codeBlock) classes.push("cm-md-code");
  if (context.rule) classes.push("cm-md-hr");
  if (context.item) classes.push("cm-md-list-item");
  if (classes.length > 0) {
    decorations.push(Decoration.line({ class: classes.join(" ") }).range(line.from));
  }

  if (heading) {
    // 闭合式 ATX 标题（# 标题 #）有两个 HeaderMark，都要处理
    for (const mark of heading.getChildren("HeaderMark")) {
      const markLine = state.doc.lineAt(mark.from);
      if (markLine.number !== line.number) continue;
      if (touches(state, markLine.from, markLine.to)) {
        decorations.push(Decoration.mark({ class: MARKER_CLASS }).range(mark.from, mark.to));
      } else if (heading.name.startsWith("Setext")) {
        decorations.push(Decoration.replace({}).range(mark.from, mark.to));
      } else {
        let end = mark.to;
        while (
          end < line.to &&
          (state.doc.sliceString(end, end + 1) === " " ||
            state.doc.sliceString(end, end + 1) === "\t")
        ) {
          end += 1;
        }
        decorations.push(Decoration.replace({}).range(mark.from, end));
      }
    }
  }

  const item = context.item;
  if (item) {
    const mark = item.getChild("ListMark");
    if (mark && mark.from >= lineFrom && mark.to <= lineTo) {
      const ordered = item.parent?.name === "OrderedList";
      if (ordered || touches(state, lineFrom, lineTo)) {
        // 有序编号保留原文（重新编号由 CodeMirror 负责），无序标记在光标离开时变圆点
        decorations.push(Decoration.mark({ class: MARKER_CLASS }).range(mark.from, mark.to));
      } else {
        decorations.push(Decoration.replace({ widget: bulletWidget }).range(mark.from, mark.to));
      }
    }
  }

  if (context.rule) {
    if (revealed) {
      decorations.push(
        Decoration.mark({ class: MARKER_CLASS }).range(context.rule.from, context.rule.to),
      );
    } else {
      // 只替换分隔线本身，避免和引用标记的替换区间重叠
      decorations.push(Decoration.replace({}).range(context.rule.from, context.rule.to));
    }
  }

  const inFence = context.fence !== null;
  tree.iterate({
    from: lineFrom,
    to: lineTo,
    enter: (ref) => {
      if (ref.name === "QuoteMark") {
        if (revealed) {
          decorations.push(Decoration.mark({ class: MARKER_CLASS }).range(ref.from, ref.to));
        } else {
          let end = ref.to;
          while (
            end < lineTo &&
            (state.doc.sliceString(end, end + 1) === " " ||
              state.doc.sliceString(end, end + 1) === "\t")
          ) {
            end += 1;
          }
          decorations.push(Decoration.replace({}).range(ref.from, end));
        }
      } else if (inFence && (ref.name === "CodeMark" || ref.name === "CodeInfo")) {
        // 围栏标记不加入 atomicRanges：否则方向键无法进入隐藏行触发揭示
        hideOrMark(ref.node, revealed, decorations, atomic, false);
      }
      return undefined;
    },
  });
}

function addInlineDecorations(
  state: EditorState,
  tree: ReturnType<typeof syntaxTree>,
  from: number,
  to: number,
  decorations: Range<Decoration>[],
  atomic: Range<Decoration>[],
): void {
  tree.iterate({
    from,
    to,
    enter: (ref) => {
      const markerNames = INLINE_MARKERS[ref.name];
      if (markerNames) {
        const node = ref.node;
        const revealed = touches(state, node.from, node.to);
        decorations.push(
          Decoration.mark({
            class:
              ref.name === "StrongEmphasis"
                ? "cm-md-strong"
                : ref.name === "Emphasis"
                  ? "cm-md-em"
                  : ref.name === "Strikethrough"
                    ? "cm-md-strike"
                    : "cm-md-code-inline",
          }).range(node.from, node.to),
        );
        for (let child = node.firstChild; child; child = child.nextSibling) {
          if (markerNames.includes(child.name)) {
            hideOrMark(child, revealed, decorations, atomic);
          }
        }
        // 行内代码内部是字面量，不再继续遍历
        return ref.name === "InlineCode" ? false : undefined;
      }

      if (ref.name === "Link") {
        const node = ref.node;
        const revealed = touches(state, node.from, node.to);
        decorations.push(Decoration.mark({ class: "cm-md-link" }).range(node.from, node.to));
        const first = node.firstChild;
        const autolink =
          first?.name === "LinkMark" && state.doc.sliceString(first.from, first.to) === "<";
        for (let child = node.firstChild; child; child = child.nextSibling) {
          if (child.name === "LinkMark") {
            hideOrMark(child, revealed, decorations, atomic);
          } else if (
            !autolink &&
            (child.name === "URL" || child.name === "LinkTitle" || child.name === "LinkLabel")
          ) {
            hideOrMark(child, revealed, decorations, atomic);
          }
        }
        return false;
      }

      if (ref.name === "Autolink") {
        const node = ref.node;
        const revealed = touches(state, node.from, node.to);
        decorations.push(Decoration.mark({ class: "cm-md-link" }).range(node.from, node.to));
        for (let child = node.firstChild; child; child = child.nextSibling) {
          if (child.name === "LinkMark") {
            hideOrMark(child, revealed, decorations, atomic);
          }
        }
        return false;
      }

      if (ref.name === "URL") {
        decorations.push(Decoration.mark({ class: "cm-md-link" }).range(ref.from, ref.to));
        return false;
      }

      // 代码块 / HTML / 图片内部不做行内渲染
      if (ref.name === "FencedCode" || ref.name === "CodeBlock" || ref.name === "HTMLBlock") {
        return false;
      }
      if (ref.name === "Image") return false;
      return undefined;
    },
  });
}

interface PreviewBuild {
  decorations: DecorationSet;
  atomicRanges: DecorationSet;
}

function buildPreview(
  state: EditorState,
  ranges: readonly { from: number; to: number }[],
): PreviewBuild {
  const end = ranges.length > 0 ? ranges[ranges.length - 1].to : state.doc.length;
  const tree = ensureSyntaxTree(state, end, 50) ?? syntaxTree(state);
  const decorations: Range<Decoration>[] = [];
  const atomic: Range<Decoration>[] = [];
  const seenLines = new Set<number>();

  for (const range of ranges) {
    const from = Math.max(0, Math.min(range.from, state.doc.length));
    const to = Math.max(from, Math.min(range.to, state.doc.length));
    const first = state.doc.lineAt(from).number;
    const last = state.doc.lineAt(to).number;
    for (let number = first; number <= last; number++) {
      if (seenLines.has(number)) continue;
      seenLines.add(number);
      const line = state.doc.line(number);
      addLineDecorations(state, tree, line.from, line.to, decorations, atomic);
    }
  }

  for (const range of ranges) {
    addInlineDecorations(state, tree, range.from, range.to, decorations, atomic);
  }

  return {
    decorations: Decoration.set(decorations, true),
    atomicRanges: Decoration.set(atomic, true),
  };
}

/** 供测试与源码模式使用：整篇文档的预览装饰。 */
export function buildPreviewDecorations(
  state: EditorState,
  ranges?: readonly { from: number; to: number }[],
): DecorationSet {
  return buildPreview(state, ranges ?? [{ from: 0, to: state.doc.length }]).decorations;
}

export function buildPreviewAtomicRanges(
  state: EditorState,
  ranges?: readonly { from: number; to: number }[],
): DecorationSet {
  return buildPreview(state, ranges ?? [{ from: 0, to: state.doc.length }]).atomicRanges;
}

class BulletWidget extends WidgetType {
  toDOM(): HTMLElement {
    const span = document.createElement("span");
    span.className = "cm-md-bullet";
    span.textContent = "•";
    return span;
  }

  eq(): boolean {
    return true;
  }

  ignoreEvent(): boolean {
    return false;
  }
}

const bulletWidget = new BulletWidget();

class LivePreviewPlugin {
  decorations: DecorationSet = Decoration.none;
  atomicRanges: DecorationSet = Decoration.none;

  private readonly view: EditorView;
  private readonly abort = new AbortController();
  private frozen = false;
  private destroyed = false;
  private releaseTimer: number | undefined;

  constructor(view: EditorView) {
    this.view = view;
    this.rebuild();
    const { signal } = this.abort;
    // 捕获阶段先于 CodeMirror 自己的 mousedown 选区更新
    view.dom.addEventListener("pointerdown", () => this.freeze(), { capture: true, signal });
    window.addEventListener("pointerup", () => this.scheduleRelease(), { capture: true, signal });
    window.addEventListener("pointercancel", () => this.scheduleRelease(), {
      capture: true,
      signal,
    });
    view.contentDOM.addEventListener("keydown", () => this.release(true), {
      capture: true,
      signal,
    });
    view.contentDOM.addEventListener("compositionstart", () => this.freeze(), { signal });
    view.contentDOM.addEventListener(
      "compositionend",
      () => window.setTimeout(() => this.release(true), 0),
      { signal },
    );
  }

  update(update: ViewUpdate): void {
    if (this.frozen || update.view.composing) {
      if (update.docChanged) {
        this.decorations = this.decorations.map(update.changes);
        this.atomicRanges = this.atomicRanges.map(update.changes);
      }
      return;
    }
    const forced = update.transactions.some((transaction) =>
      transaction.effects.some((effect) => effect.is(forcePreviewRebuild)),
    );
    // 大文档的解析可能分多次完成；语法树推进时也要重算，否则可见区会停在源码态
    const treeChanged = syntaxTree(update.startState) !== syntaxTree(update.state);
    if (
      update.docChanged ||
      update.selectionSet ||
      update.viewportChanged ||
      forced ||
      treeChanged
    ) {
      this.rebuild();
    }
  }

  destroy(): void {
    this.destroyed = true;
    if (this.releaseTimer !== undefined) window.clearTimeout(this.releaseTimer);
    this.abort.abort();
  }

  private rebuild(): void {
    if (this.destroyed) return;
    const result = buildPreview(this.view.state, this.view.visibleRanges);
    this.decorations = result.decorations;
    this.atomicRanges = result.atomicRanges;
  }

  private freeze(): void {
    if (this.releaseTimer !== undefined) {
      window.clearTimeout(this.releaseTimer);
      this.releaseTimer = undefined;
    }
    this.frozen = true;
  }

  private scheduleRelease(): void {
    if (!this.frozen) return;
    if (this.releaseTimer !== undefined) window.clearTimeout(this.releaseTimer);
    this.releaseTimer = window.setTimeout(() => this.release(true), MOUSE_REVEAL_DELAY_MS);
  }

  private release(force: boolean): void {
    if (this.releaseTimer !== undefined) {
      window.clearTimeout(this.releaseTimer);
      this.releaseTimer = undefined;
    }
    if (this.destroyed || !this.frozen) return;
    this.frozen = false;
    if (force && !this.view.composing) {
      this.view.dispatch({ effects: forcePreviewRebuild.of(null) });
    }
  }
}

const livePreviewPlugin = ViewPlugin.fromClass(LivePreviewPlugin, {
  decorations: (plugin) => plugin.decorations,
});

/** Live Preview 扩展组；源码模式通过 Compartment 整体替换为空数组。 */
export function livePreviewExtensions(): Extension {
  return [
    livePreviewPlugin,
    EditorView.atomicRanges.of(
      (view) => view.plugin(livePreviewPlugin)?.atomicRanges ?? Decoration.none,
    ),
  ];
}
