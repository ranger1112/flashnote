import { ui } from "./dom";
import { focusEditor } from "./editor";
import type { NoteEntry } from "./lib";
import { toast } from "./toast";

/** 续写目标：content 是磁盘上的当前正文，用作 update_note_entry 的定位依据。 */
export interface ContinuationTarget {
  path: string;
  date: string;
  time: string;
  content: string;
}

let target: ContinuationTarget | null = null;

export function activeContinuation(): ContinuationTarget | null {
  return target;
}

export function startContinuation(note: NoteEntry): void {
  target = {
    path: note.path,
    date: note.date,
    time: note.time,
    content: note.content,
  };
  paintContinuationBar();
}

/** 更新成功后必须同步定位依据，否则下一次保存会找不到原记录。 */
export function updateContinuationContent(content: string): void {
  if (target) target.content = content;
}

export function endContinuation(): void {
  target = null;
  paintContinuationBar();
}

export function paintContinuationBar(): void {
  ui.continuationBar.hidden = target === null;
  ui.continuationLabel.textContent = target
    ? `正在续写 ${target.date} ${target.time} 的记录 · 保存后更新原记录`
    : "";
}

export function initContinuation(): void {
  ui.continuationExit.addEventListener("click", () => {
    if (!target) return;
    endContinuation();
    toast("已退出续写，之后的保存会新建记录", false, 1600);
    focusEditor();
  });
  paintContinuationBar();
}
