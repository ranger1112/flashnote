import { EditorSelection, EditorState, Compartment, Transaction } from "@codemirror/state";
import { EditorView, drawSelection, dropCursor, keymap, placeholder } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { indentUnit } from "@codemirror/language";
import {
  deleteMarkupBackward,
  insertNewlineContinueMarkupCommand,
  markdown,
  markdownLanguage,
} from "@codemirror/lang-markdown";
import { indentListLess, indentListMore } from "./editor-commands";
import { livePreviewExtensions } from "./live-preview";

export interface SetTextOptions {
  caret?: "start" | "end" | "preserve";
}

export interface CaptureEditor {
  readonly kind: "codemirror" | "textarea";
  getText(): string;
  setText(text: string, options?: SetTextOptions): void;
  focus(): void;
  destroy(): void;
}

export interface InitEditorOptions {
  mount: HTMLElement;
  onChange: () => void;
}

const editorTheme = EditorView.theme({
  "&": { height: "100%", color: "var(--text)", backgroundColor: "transparent" },
  "&.cm-focused": { outline: "none" },
  ".cm-scroller": {
    fontFamily: '"Segoe UI", "Microsoft YaHei UI", "PingFang SC", sans-serif',
    fontSize: "15px",
    lineHeight: "1.7",
    padding: "8px 10px",
    overflow: "auto",
  },
  ".cm-content": {
    padding: "0",
    caretColor: "var(--accent)",
    userSelect: "text",
  },
  ".cm-line": { padding: "0" },
  ".cm-placeholder": { color: "var(--muted)", opacity: "0.7" },
  // drawSelection 的 baseTheme 选区色优先级较高，这里统一覆盖成主题强调色
  ".cm-selectionLayer .cm-selectionBackground": {
    backgroundColor: "var(--selection) !important",
  },
  ".cm-cursor": { borderLeftColor: "var(--accent)" },
});

/**
 * CodeMirror 依赖真实布局能力；在 happy-dom 这类没有排版的环境里直接挂载
 * 会在测量阶段抛错。探测失败时退回同尺寸的 textarea，保证速记本身仍可用。
 */
function canMountCodeMirror(): boolean {
  const probe = document.createElement("div");
  probe.style.cssText = "position:absolute;left:-10000px;top:0;width:10px;height:10px;";
  document.body.append(probe);
  const height = probe.getBoundingClientRect().height;
  probe.remove();
  return height > 0;
}

class TextareaEditor implements CaptureEditor {
  readonly kind = "textarea" as const;
  private readonly textarea: HTMLTextAreaElement;

  constructor(mount: HTMLElement, onChange: () => void) {
    const textarea = document.createElement("textarea");
    textarea.id = "note";
    textarea.className = "note-fallback";
    textarea.setAttribute("aria-label", "速记内容");
    textarea.placeholder = "想到什么就写什么…";
    textarea.spellcheck = false;
    textarea.addEventListener("input", onChange);
    mount.append(textarea);
    this.textarea = textarea;
  }

  getText(): string {
    return this.textarea.value;
  }

  setText(text: string, options: SetTextOptions = {}): void {
    this.textarea.value = text;
    if (options.caret === "end") {
      this.textarea.setSelectionRange(text.length, text.length);
    } else if (options.caret === "start") {
      this.textarea.setSelectionRange(0, 0);
    }
  }

  focus(): void {
    this.textarea.focus();
  }

  destroy(): void {
    this.textarea.remove();
  }
}

export class CodeMirrorEditor implements CaptureEditor {
  readonly kind = "codemirror" as const;
  /** 暴露给测试与调试；生产代码通过下面的门面函数访问。 */
  readonly view: EditorView;
  private readonly onChange: () => void;
  private readonly sourceCompartment = new Compartment();
  private sourceEnabled = false;
  private programmatic = false;
  private destroyed = false;

  constructor(mount: HTMLElement, onChange: () => void) {
    this.onChange = onChange;
    const state = EditorState.create({
      doc: "",
      extensions: [
        history(),
        drawSelection(),
        dropCursor(),
        EditorView.lineWrapping,
        EditorView.contentAttributes.of({
          "aria-label": "速记内容",
          spellcheck: "false",
        }),
        editorTheme,
        indentUnit.of("  "),
        markdown({ base: markdownLanguage, addKeymap: false }),
        keymap.of([
          {
            key: "Enter",
            run: insertNewlineContinueMarkupCommand({ nonTightLists: false }),
          },
          { key: "Backspace", run: deleteMarkupBackward },
          { key: "Tab", run: indentListMore, shift: indentListLess },
          ...historyKeymap,
          ...defaultKeymap,
        ]),
        placeholder("想到什么就写什么…"),
        this.sourceCompartment.of(livePreviewExtensions()),
        EditorView.updateListener.of((update) => {
          if (update.docChanged && !this.programmatic) this.onChange();
        }),
      ],
    });
    this.view = new EditorView({ state, parent: mount });
  }

  getText(): string {
    return this.view.state.doc.toString();
  }

  setText(text: string, options: SetTextOptions = {}): void {
    const caret = options.caret ?? "preserve";
    const position =
      caret === "end"
        ? text.length
        : caret === "start"
          ? 0
          : Math.min(this.view.state.selection.main.head, text.length);
    this.programmatic = true;
    try {
      this.view.dispatch({
        changes: { from: 0, to: this.view.state.doc.length, insert: text },
        selection: EditorSelection.cursor(position),
        annotations: Transaction.addToHistory.of(false),
        // 长记录续写/草稿恢复后光标在末尾：没有这一步视口会停在顶部，
        // focus() 又用了 preventScroll，用户看不到光标。
        scrollIntoView: caret !== "preserve",
      });
    } finally {
      this.programmatic = false;
    }
  }

  focus(): void {
    this.view.focus();
  }

  toggleSourceMode(): boolean {
    this.sourceEnabled = !this.sourceEnabled;
    this.view.dispatch({
      effects: this.sourceCompartment.reconfigure(
        this.sourceEnabled ? [] : livePreviewExtensions(),
      ),
    });
    return this.sourceEnabled;
  }

  destroy(): void {
    if (this.destroyed) return;
    this.destroyed = true;
    this.view.destroy();
  }
}

let activeEditor: CaptureEditor | null = null;

export function initEditor(options: InitEditorOptions): CaptureEditor {
  activeEditor?.destroy();
  options.mount.replaceChildren();

  if (canMountCodeMirror()) {
    try {
      activeEditor = new CodeMirrorEditor(options.mount, options.onChange);
      return activeEditor;
    } catch (error) {
      console.warn("CodeMirror 挂载失败，改用纯文本编辑器", error);
      options.mount.replaceChildren();
    }
  }

  activeEditor = new TextareaEditor(options.mount, options.onChange);
  return activeEditor;
}

export function getEditor(): CaptureEditor | null {
  return activeEditor;
}

export function getText(): string {
  return activeEditor?.getText() ?? "";
}

export function setText(text: string, options?: SetTextOptions): void {
  activeEditor?.setText(text, options);
}

export function focusEditor(): void {
  activeEditor?.focus();
}

export function toggleSourceMode(): boolean {
  return activeEditor instanceof CodeMirrorEditor ? activeEditor.toggleSourceMode() : false;
}
