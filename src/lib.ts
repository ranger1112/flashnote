/**
 * 与 DOM 无关的纯函数，单独放一份便于单元测试（见 lib.test.ts）。
 */

export interface NoteEntry {
  date: string;
  time: string;
  content: string;
  source: string | null;
  /** 正文里内联的 `#标签`，后端解析后按首次出现顺序去重。 */
  tags: string[];
  path: string;
}

/** KeyboardEvent 里跟快捷键录制有关的字段，测试时可以直接构造。 */
export type AccelEvent = Pick<
  KeyboardEvent,
  "code" | "key" | "ctrlKey" | "altKey" | "shiftKey" | "metaKey"
>;

/** 把 `ctrl+alt+n` 显示成 `Ctrl + Alt + N`。 */
export function formatAccel(accel: string): string {
  return accel
    .split("+")
    .map((part) => {
      const value = part.trim();
      const lower = value.toLowerCase();
      if (lower === "ctrl" || lower === "control") return "Ctrl";
      if (lower === "alt") return "Alt";
      if (lower === "shift") return "Shift";
      if (lower === "super" || lower === "meta") return "Win";
      if (value.length === 1) return value.toUpperCase();
      return value;
    })
    .join(" + ");
}

/**
 * 从按键事件还原成 Tauri 的加速键字符串。
 * 没有修饰键、或只按下了修饰键本身时返回 null（调用方应提示用户）。
 */
export function accelFromEvent(event: AccelEvent): string | null {
  const mods: string[] = [];
  if (event.ctrlKey) mods.push("ctrl");
  if (event.altKey) mods.push("alt");
  if (event.shiftKey) mods.push("shift");
  if (event.metaKey) mods.push("super");

  if (["Control", "Alt", "Shift", "Meta"].includes(event.key)) return null;

  let key: string | null = null;
  if (/^Key[A-Z]$/.test(event.code)) key = event.code.slice(3).toLowerCase();
  else if (/^Digit[0-9]$/.test(event.code)) key = event.code.slice(5);
  else if (/^F([1-9]|1[0-9]|2[0-4])$/.test(event.code)) key = event.code.toLowerCase();
  else if (event.code === "Space") key = "space";
  else if (event.code === "Backquote") key = "`";
  else if (event.code === "Minus") key = "-";
  else if (event.code === "Equal") key = "=";
  else if (event.key.length === 1) key = event.key.toLowerCase();

  if (!key || mods.length === 0) return null;
  return [...mods, key].join("+");
}

export type ThemeMode = "system" | "light" | "dark";
export type ResolvedTheme = "light" | "dark";

export const MIN_OPACITY = 0.6;
export const MAX_OPACITY = 1;

/** 后端或旧配置给出的任意值，收敛到支持的三档主题。 */
export function normalizeTheme(value: unknown): ThemeMode {
  if (typeof value !== "string") return "system";
  const mode = value.trim().toLowerCase();
  return mode === "light" || mode === "dark" ? mode : "system";
}

/** 「跟随系统」按当前系统偏好解析成具体主题。 */
export function resolveTheme(mode: ThemeMode, prefersLight: boolean): ResolvedTheme {
  if (mode === "system") return prefersLight ? "light" : "dark";
  return mode;
}

/** 背景不透明度夹到 60%–100%；非法数字按不透明处理。 */
export function clampOpacity(value: number): number {
  if (!Number.isFinite(value)) return MAX_OPACITY;
  return Math.min(MAX_OPACITY, Math.max(MIN_OPACITY, value));
}

/**
 * 搜索词是否命中一条笔记（调用方传入已小写化的词）。
 *
 * `#标签` 走精确匹配：否则搜 `#工作` 会把 `#工作台` 也带出来。
 * 其余情况按日期、时间、正文、来源做子串匹配。
 */
export function matchesTerm(
  note: Pick<NoteEntry, "date" | "time" | "content" | "source" | "tags">,
  term: string,
): boolean {
  const tag = exactTagTerm(term);
  if (tag !== null) {
    return note.tags.some((value) => value.toLowerCase() === tag.toLowerCase());
  }
  return [note.date, note.time, note.content, note.source ?? ""].some((value) =>
    value.toLowerCase().includes(term),
  );
}

/** 搜索词是不是「就是要这个标签」：单个 `#标签`，且不带空格。 */
export function exactTagTerm(term: string): string | null {
  const value = term.trim();
  if (!value.startsWith("#") || value.length < 2) return null;
  return value.includes(" ") || value.includes("\t") ? null : value.slice(1);
}

/** 两条笔记是否同一条：与后端 delete_note_entry 的定位参数保持一致。 */
export function sameEntry(left: NoteEntry, right: NoteEntry): boolean {
  return (
    left.path === right.path &&
    left.date === right.date &&
    left.time === right.time &&
    left.content === right.content
  );
}

/** 从列表里剔掉指定条目，用于过滤服务端返回的、尚未落盘的待删除项。 */
export function withoutEntry<T extends NoteEntry>(list: T[], target: NoteEntry): T[] {
  return list.filter((note) => !sameEntry(note, target));
}

/**
 * 分页偏移推进：reset 时等于本批行数，否则累加。
 * 用服务端返回的行数而不是渲染后的条数，避免客户端过滤把偏移算小、导致重复拉取。
 */
export function nextOffset(previous: number, batchLength: number, reset: boolean): number {
  return reset ? batchLength : previous + batchLength;
}

/** 预览里只把 http(s)/mailto 链接交给系统浏览器，其余协议一律不动。 */
export function externalLinkTarget(href: string): string | null {
  const value = href.trim();
  return /^(https?:\/\/|mailto:)/i.test(value) ? value : null;
}
