import { invoke } from "@tauri-apps/api/core";
import { restoreContinuation, syncContinuationDraft } from "./continuation";
import { getText, setText } from "./editor";

export const DRAFT_KEY = "flashnote.draft";

let draftTimer: number | undefined;

export function scheduleDraftSave(): void {
  if (draftTimer !== undefined) window.clearTimeout(draftTimer);
  draftTimer = window.setTimeout(() => {
    draftTimer = undefined;
    void persistDraft();
  }, 500);
}

export async function persistDraft(): Promise<void> {
  const content = getText();
  // 草稿与续写目标要一起落盘，重启后才能确认这份正文属于哪个续写目标
  syncContinuationDraft(content);
  try {
    await invoke("save_draft", { content });
  } catch {
    // 草稿写入失败不打断输入，下一次输入会重试
  }
}

export async function flushDraft(): Promise<void> {
  if (draftTimer !== undefined) {
    window.clearTimeout(draftTimer);
    draftTimer = undefined;
  }
  await persistDraft();
}

/** 保存成功后清掉排队的草稿、本地旧副本与服务端草稿。 */
export function clearStoredDraft(): void {
  if (draftTimer !== undefined) {
    window.clearTimeout(draftTimer);
    draftTimer = undefined;
  }
  localStorage.removeItem(DRAFT_KEY);
  void invoke("clear_draft").catch(() => {});
}

/** 启动时读取磁盘草稿；首次升级时把 localStorage 中的旧草稿迁移过来。 */
export async function restoreDraft(): Promise<void> {
  const legacyDraft = localStorage.getItem(DRAFT_KEY);
  const draft = await invoke<string>("load_draft");
  // 读取期间用户可能已经开始输入，不要用磁盘内容把它盖掉
  if (getText() === "") {
    if (draft) {
      setText(draft, { caret: "end" });
      restoreContinuation(draft);
    } else if (legacyDraft) {
      setText(legacyDraft, { caret: "end" });
      restoreContinuation(legacyDraft);
    }
  }

  if (legacyDraft !== null) {
    if (draft) {
      // 磁盘上已有更新的草稿，旧副本可以丢弃
      localStorage.removeItem(DRAFT_KEY);
    } else {
      // 迁移成功才删 localStorage；失败时保留，下次启动还能重试
      try {
        await invoke("save_draft", { content: legacyDraft });
        localStorage.removeItem(DRAFT_KEY);
      } catch {
        // 保留 localStorage 里的旧草稿
      }
    }
  }
}
