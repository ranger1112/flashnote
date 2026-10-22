/**
 * 与 DOM 无关的纯函数，单独放一份便于单元测试（见 lib.test.ts）。
 */

export interface NoteEntry {
  date: string;
  time: string;
  content: string;
  source: string | null;
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

/** 历史面板的本地过滤：日期、时间、正文、来源都参与匹配（调用方传入小写化的词）。 */
export function matchesTerm(
  note: Pick<NoteEntry, "date" | "time" | "content" | "source">,
  term: string,
): boolean {
  return [note.date, note.time, note.content, note.source ?? ""].some((value) =>
    value.toLowerCase().includes(term),
  );
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
