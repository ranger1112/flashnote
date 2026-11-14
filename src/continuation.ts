import { invoke } from "@tauri-apps/api/core";
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

/** 续写状态随草稿一起跨重启保留：否则重启后正文会变成没有目标的普通草稿。 */
const STORAGE_KEY = "flashnote.continuation";

interface StoredTarget extends ContinuationTarget {
  /** 最后一次落盘的草稿内容；只有草稿与目标对得上才恢复，避免把 A 的草稿写进 B。 */
  draft: string;
}

let target: ContinuationTarget | null = null;
let draftSnapshot: string | null = null;

function persist(): void {
  try {
    if (target && draftSnapshot !== null) {
      localStorage.setItem(STORAGE_KEY, JSON.stringify({ ...target, draft: draftSnapshot }));
    } else {
      localStorage.removeItem(STORAGE_KEY);
    }
  } catch {
    // 隐私模式或配额不足：只影响重启恢复，不影响本次续写
  }
}

function isTarget(value: unknown): value is StoredTarget {
  if (!value || typeof value !== "object") return false;
  const record = value as Record<string, unknown>;
  return (
    ["path", "date", "time", "content", "draft"].every(
      (key) => typeof record[key] === "string",
    )
  );
}

/** 草稿落盘时同步记录正文，重启后只有与草稿一致的目标才会恢复。 */
export function syncContinuationDraft(content: string): void {
  if (!target) return;
  draftSnapshot = content;
  persist();
}

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
  // 新目标还没有对应的草稿：等下一次草稿落盘时再写入快照
  draftSnapshot = null;
  persist();
  paintContinuationBar();
}

/** 更新成功后必须同步定位依据，否则下一次保存会找不到原记录。 */
export function updateContinuationContent(content: string): void {
  if (target) {
    target.content = content;
    persist();
  }
}

/**
 * 原记录可能已被历史面板、外部编辑器或同步改写：重新读取同一时间点的条目，
 * 只有唯一命中时才刷新定位依据，避免把另一条同时间的记录当成目标。
 */
export async function refreshContinuationContent(current: ContinuationTarget): Promise<boolean> {
  try {
    const entries = await invoke<NoteEntry[]>("get_note_entries", { path: current.path });
    const matches = entries.filter((entry) => entry.time === current.time);
    if (matches.length !== 1) return false;
    current.content = matches[0].content;
    if (target === current) persist();
    return true;
  } catch {
    return false;
  }
}

export function endContinuation(): void {
  target = null;
  draftSnapshot = null;
  persist();
  paintContinuationBar();
}

/** 草稿恢复后调用：只有快照与刚恢复的草稿一致时才恢复续写目标。 */
export function restoreContinuation(loadedDraft: string): boolean {
  if (target) return true;
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return false;
    const parsed: unknown = JSON.parse(raw);
    if (!isTarget(parsed) || parsed.draft !== loadedDraft) {
      localStorage.removeItem(STORAGE_KEY);
      return false;
    }
    target = {
      path: parsed.path,
      date: parsed.date,
      time: parsed.time,
      content: parsed.content,
    };
    draftSnapshot = loadedDraft;
    paintContinuationBar();
    return true;
  } catch {
    return false;
  }
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
