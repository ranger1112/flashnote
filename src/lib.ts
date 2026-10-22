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

/** 历史面板的本地过滤：日期、时间、正文、来源都参与匹配（调用方传入小写化的词）。 */
export function matchesTerm(
  note: Pick<NoteEntry, "date" | "time" | "content" | "source">,
  term: string,
): boolean {
  return [note.date, note.time, note.content, note.source ?? ""].some((value) =>
    value.toLowerCase().includes(term),
  );
}
