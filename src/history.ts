import { invoke } from "@tauri-apps/api/core";
import {
  exactTagTerm,
  matchesLiteralTag,
  matchesTerm,
  nextOffset,
  withoutEntry,
  type NoteEntry,
} from "./lib";
import { markdownHtml } from "./markdown";
import { ui } from "./dom";
import { settings } from "./settings-store";
import { hideUndoBar, showUndoBar, toast } from "./toast";
import { hidePanel, showPanel } from "./panels";
import { markSyncPending, subscribeSyncStatus } from "./sync";

const PAGE_SIZE = 100;
const SEARCH_LIMIT = 200;

export interface HistoryHooks {
  onContinue: (note: NoteEntry) => void;
}

let historyHooks: HistoryHooks = { onContinue: () => {} };

let searchTimer: number | undefined;
let notes: NoteEntry[] = [];
let noteOffset = 0;
let hasMoreNotes = false;
let serverFiltered = false;
let searchSeq = 0;
/** 历史面板最近一次读取失败的提示；为空表示没有错误。 */
let historyError: string | null = null;
/** 日期 → 文件级同步状态（`synced` / `pending` / `conflict`）。 */
let fileStates = new Map<string, string>();
let fileStatesSeq = 0;

const DATE_SYNC_LABEL: Record<string, string> = {
  synced: "已同步",
  pending: "待同步",
  conflict: "有冲突副本",
};

const DATE_SYNC_TITLE: Record<string, string> = {
  synced: "这一天的文件与云端一致",
  pending: "这一天的文件有改动，等待下一轮同步",
  conflict: "这一天存在冲突副本，需要手动合并",
};

/** 列表里出现的日期，按出现顺序去重；后端只算这些文件。 */
function visibleDates(): string[] {
  const seen = new Set<string>();
  for (const note of notes) seen.add(note.date);
  return [...seen];
}

function paintFileStates(): void {
  ui.entryList.querySelectorAll<HTMLElement>(".entry-date").forEach((row) => {
    const badge = row.querySelector<HTMLElement>(".date-sync");
    if (!badge) return;
    const state = fileStates.get(row.dataset.date ?? "");
    badge.textContent = state ? (DATE_SYNC_LABEL[state] ?? "") : "";
    badge.className = state ? `date-sync ${state}` : "date-sync";
    badge.title = state ? (DATE_SYNC_TITLE[state] ?? "") : "";
  });
}

/** 只刷新当前列表涉及的日期，避免每次打开面板都哈希整个笔记目录。 */
async function refreshFileStates(): Promise<void> {
  const dates = visibleDates();
  if (dates.length === 0) {
    fileStates.clear();
    return;
  }
  const seq = ++fileStatesSeq;
  try {
    const states = await invoke<{ date: string; state: string }[]>("get_sync_file_states", {
      dates,
    });
    if (seq !== fileStatesSeq) return;
    fileStates = new Map(states.map((item) => [item.date, item.state]));
    paintFileStates();
  } catch (error) {
    // 同步状态只是辅助信息，读不到不影响历史列表本身
    console.error("读取文件同步状态失败", error);
  }
}

function dateLabel(date: string): string {
  const now = new Date();
  const today = [
    now.getFullYear(),
    String(now.getMonth() + 1).padStart(2, "0"),
    String(now.getDate()).padStart(2, "0"),
  ].join("-");
  return date === today ? `今天 · ${date}` : date;
}

/**
 * 上锁时调用：取消搜索与尚未落盘的删除，避免锁定后定时器再触发被拦截的命令。
 * 历史面板已经关闭，不需要重绘；解锁后重新打开会重新加载。
 */
export function suspendHistoryForLock(): void {
  if (searchTimer !== undefined) {
    window.clearTimeout(searchTimer);
    searchTimer = undefined;
  }
  searchSeq += 1;
  if (pendingDelete) {
    const pending = pendingDelete;
    pendingDelete = null;
    window.clearTimeout(pending.timer);
    hideUndoBar();
    // 把条目放回内存列表，但不渲染：面板关闭，且渲染会调用被锁拦截的同步状态命令。
    notes.splice(Math.min(pending.index, notes.length), 0, pending.note);
  }
}

export function closeHistory(animate = true, force = false): void {  if (ui.historyPanel.classList.contains("hidden")) return;
  // 编辑框里有未保存的改动：直接关闭会静默丢掉，必须先保存或取消。
  // 收起窗口（force）例外，否则用户没法用快捷键收起小窗。
  if (!force && hasOpenEditor()) {
    toast("请先保存或取消编辑", true);
    return;
  }
  // 面板关了就不该再跑一次搜索
  if (searchTimer !== undefined) {
    window.clearTimeout(searchTimer);
    searchTimer = undefined;
  }
  // 未落盘的删除由撤销定时器负责，收起面板不提前提交，撤销窗口保持完整
  hidePanel(ui.historyPanel, animate);
}

export function paintPreviewButton(): void {
  ui.historyPreview.setAttribute("aria-pressed", String(settings.historyPreview));
  ui.historyPreview.textContent = settings.historyPreview ? "原文" : "预览";
}

interface PendingDelete {
  note: NoteEntry;
  index: number;
  timer: number;
}

let pendingDelete: PendingDelete | null = null;

function bindDelete(button: HTMLButtonElement, note: NoteEntry): void {
  button.addEventListener("click", () => requestDelete(note));
}

/** 待删除的条目还没写盘，服务端返回的列表里仍然有它，渲染前先剔掉。 */
function dropPendingDelete(list: NoteEntry[]): NoteEntry[] {
  const pending = pendingDelete;
  if (!pending) return list;
  return withoutEntry(list, pending.note);
}

function dropFromNotes(note: NoteEntry): void {
  notes = withoutEntry(notes, note);
}

/** 先从列表移除并给 5 秒撤销窗口，到期才真正写盘。 */
function requestDelete(note: NoteEntry): void {
  if (hasOpenEditor()) {
    toast("请先保存或取消正在编辑的笔记", true);
    return;
  }
  // 同时只保留一条待删除记录，前一条立即落盘
  if (pendingDelete) void commitPendingDelete();

  const index = notes.indexOf(note);
  if (index < 0) return;
  notes.splice(index, 1);
  renderNotes();

  const timer = window.setTimeout(() => void commitPendingDelete(), 5000);
  pendingDelete = { note, index, timer };
  showUndoBar("已删除");
}

async function commitPendingDelete(): Promise<void> {
  const pending = pendingDelete;
  if (!pending) return;
  pendingDelete = null;
  window.clearTimeout(pending.timer);
  hideUndoBar();

  try {
    await invoke("delete_note_entry", {
      path: pending.note.path,
      date: pending.note.date,
      time: pending.note.time,
      content: pending.note.content,
    });
    markSyncPending();
    // 删除完成前发出的列表请求，返回结果里还带着这条笔记，直接作废
    searchSeq += 1;
    // 服务端少了一条，后续分页偏移要跟着回退，否则「加载更多」会跳过一条
    if (!serverFiltered) noteOffset = Math.max(0, noteOffset - 1);
    // 兜底：期间若有陈旧结果把它塞回列表，这里再剔一次
    dropFromNotes(pending.note);
    renderNotes();
  } catch (error) {
    // 删除失败就把条目放回原位，避免界面上「看起来删掉了」
    notes.splice(Math.min(pending.index, notes.length), 0, pending.note);
    renderNotes();
    toast(String(error), true);
  }
}

function undoDelete(): void {
  const pending = pendingDelete;
  if (!pending) return;
  pendingDelete = null;
  window.clearTimeout(pending.timer);
  hideUndoBar();
  notes.splice(Math.min(pending.index, notes.length), 0, pending.note);
  renderNotes();
  toast("已恢复", false, 1200);
}

function startEdit(note: NoteEntry, contentEl: HTMLElement, editButton: HTMLButtonElement): void {
  if (contentEl.dataset.editing === "1") return;
  contentEl.dataset.editing = "1";

  const wrap = document.createElement("div");
  wrap.className = "entry-edit-wrap";

  const editor = document.createElement("textarea");
  editor.className = "entry-edit";
  editor.value = note.content;
  editor.rows = Math.min(10, Math.max(2, note.content.split("\n").length + 1));
  editor.setAttribute("aria-label", "编辑笔记内容");

  const actions = document.createElement("div");
  actions.className = "entry-edit-actions";

  const save = document.createElement("button");
  save.type = "button";
  save.className = "btn tiny primary";
  save.textContent = "保存";

  const cancel = document.createElement("button");
  cancel.type = "button";
  cancel.className = "btn tiny";
  cancel.textContent = "取消";

  let savedWhileEditing = false;
  const close = (): void => {
    wrap.replaceWith(contentEl);
    contentEl.dataset.editing = "";
    if (savedWhileEditing) {
      // 保存成功但编辑器里还有新输入，用户最终选择取消：按已保存的内容
      // 重渲列表，否则会一直显示保存前的旧文本。
      savedWhileEditing = false;
      renderNotes();
      document.querySelector<HTMLButtonElement>(".entry-action.edit")?.focus();
      return;
    }
    editButton.focus();
  };

  cancel.addEventListener("click", () => {
    // 保存请求还在飞：等它回来再关，避免把等待期间输入的文字一起丢掉。
    if (save.disabled) return;
    close();
  });

  const commit = async (): Promise<void> => {
    // 用按钮的禁用态作为唯一的「保存中」标记，避免两个状态不一致。
    if (save.disabled) return;
    const value = editor.value.trim();
    if (!value) {
      toast("内容不能为空", true);
      return;
    }
    if (value === note.content) {
      close();
      return;
    }
    save.disabled = true;
    try {
      const tags = await invoke<string[]>("update_note_entry", {
        path: note.path,
        date: note.date,
        time: note.time,
        content: note.content,
        newContent: value,
      });
      markSyncPending();
      // 只更新内存里的这条并局部重渲，保留分页 offset 与滚动位置
      note.content = value;
      // 标签由后端重新解析：正文改了标签就会变，不能用旧的
      note.tags = tags;
      // 保存期间又输入了内容：保持编辑器打开，别把新输入一起关掉。
      if (editor.value.trim() !== value) {
        savedWhileEditing = true;
        toast("已保存；还有新输入未保存", true);
        save.disabled = false;
        return;
      }
      savedWhileEditing = false;
      close();
      toast("已更新");
      renderNotes();
    } catch (error) {
      save.disabled = false;
      toast(String(error), true);
    }
  };

  save.addEventListener("click", () => void commit());
  editor.addEventListener("keydown", (event) => {
    // 输入法组合期间（如中文候选框）不处理快捷键，Esc 只应取消候选
    if (event.isComposing || event.keyCode === 229) return;
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      // 保存中不关闭：请求回来时编辑器还在，等待期间输入的文字才不会丢。
      if (!save.disabled) close();
      return;
    }
    if ((event.ctrlKey || event.metaKey) && event.key === "Enter") {
      event.preventDefault();
      void commit();
    }
  });

  actions.append(save, cancel);
  wrap.append(editor, actions);
  contentEl.replaceWith(wrap);
  editor.focus();
  editor.setSelectionRange(editor.value.length, editor.value.length);
}

/** 列表里是否有尚未提交的条目编辑器；重建列表会连同输入内容一起丢掉。 */
export function hasOpenEditor(): boolean {
  return ui.entryList.querySelector(".entry-edit") !== null;
}

function renderNotes(): void {
  if (hasOpenEditor()) return;

  const term = serverFiltered ? "" : ui.historySearch.value.trim().toLowerCase();
  const filtered = term ? notes.filter((note) => matchesLocally(note, term)) : notes;

  ui.entryList.replaceChildren();

  if (historyError !== null && filtered.length === 0) {
    // 目录不可读和「真的没有笔记」必须区分开，否则面板会长期显示空状态
    const failure = document.createElement("p");
    failure.className = "history-empty history-error";
    failure.textContent = `读取笔记失败：${historyError}`;
    const retry = document.createElement("button");
    retry.type = "button";
    retry.className = "btn";
    retry.textContent = "重试";
    retry.addEventListener("click", () => void runSearch());
    failure.append(document.createElement("br"), retry);
    ui.entryList.append(failure);
  } else if (filtered.length === 0) {
    const empty = document.createElement("p");
    empty.className = "history-empty";
    empty.textContent =
      notes.length === 0 && !serverFiltered ? "还没有记录" : "没有匹配的笔记";
    ui.entryList.append(empty);
  } else {
    let lastDate = "";
    for (const note of filtered) {
      if (note.date !== lastDate) {
        lastDate = note.date;
        const date = document.createElement("div");
        date.className = "entry-date";
        // 日期分隔行同时承载文件级同步状态（按日期文件同步，条目本身不单独同步）
        date.dataset.date = note.date;
        date.textContent = dateLabel(note.date);
        const sync = document.createElement("span");
        sync.className = "date-sync";
        date.append(sync);
        ui.entryList.append(date);
      }

      const entry = document.createElement("article");
      entry.className = "entry";

      const head = document.createElement("div");
      head.className = "entry-head";

      const time = document.createElement("span");
      time.className = "entry-time";
      time.textContent = note.time;
      head.append(time);

      if (note.tags.length > 0) {
        const tags = document.createElement("span");
        tags.className = "entry-tags";
        for (const tag of note.tags) {
          const chip = document.createElement("button");
          chip.type = "button";
          chip.className = "entry-tag";
          chip.textContent = `#${tag}`;
          chip.title = `只看 #${tag}`;
          chip.addEventListener("click", () => filterByTag(tag));
          tags.append(chip);
        }
        head.append(tags);
      }

      if (note.source) {
        const source = document.createElement("span");
        source.className = "entry-source";
        source.textContent = `来源：${note.source}`;
        head.append(source);
      }

      const actions = document.createElement("div");
      actions.className = "entry-actions";

      const continueButton = document.createElement("button");
      continueButton.type = "button";
      continueButton.className = "entry-action continue";
      continueButton.textContent = "续写";
      continueButton.title = "载入主编辑区续写，保存时更新原记录";
      continueButton.addEventListener("click", () => {
        if (hasOpenEditor()) {
          toast("请先保存或取消正在编辑的笔记", true);
          return;
        }
        historyHooks.onContinue(note);
      });

      const editButton = document.createElement("button");
      editButton.type = "button";
      editButton.className = "entry-action edit";
      editButton.textContent = "编辑";
      editButton.title = "编辑这条笔记";

      const deleteButton = document.createElement("button");
      deleteButton.type = "button";
      deleteButton.className = "entry-action danger";
      deleteButton.textContent = "删除";
      deleteButton.title = "删除这条笔记";

      actions.append(continueButton, editButton, deleteButton);

      const openButton = document.createElement("button");
      openButton.type = "button";
      openButton.className = "entry-open";
      openButton.title = "在资源管理器中定位";
      openButton.setAttribute("aria-label", "在资源管理器中定位");
      openButton.textContent = "↗";
      openButton.addEventListener("click", () => {
        void invoke("open_note_file", { path: note.path }).catch((error) =>
          toast(String(error), true),
        );
      });

      head.append(actions, openButton);

      const content = document.createElement("div");
      content.className = "entry-content";
      if (settings.historyPreview) {
        content.classList.add("markdown");
        content.innerHTML = markdownHtml(note.content);
      } else {
        content.textContent = note.content;
      }

      editButton.addEventListener("click", () => startEdit(note, content, editButton));
      bindDelete(deleteButton, note);

      entry.append(head, content);
      ui.entryList.append(entry);
    }

    if (!serverFiltered && hasMoreNotes) {
      const more = document.createElement("button");
      more.type = "button";
      more.className = "btn load-more";
      more.textContent = "加载更多";
      more.addEventListener("click", () => {
        more.disabled = true;
        more.textContent = "加载中…";
        void loadNotes(false);
      });
      ui.entryList.append(more);
    }
  }

  if (serverFiltered) {
    ui.historyCount.textContent =
      filtered.length >= SEARCH_LIMIT
        ? `前 ${filtered.length} 条结果`
        : `${filtered.length} 条结果`;
  } else if (term) {
    ui.historyCount.textContent = `${filtered.length} / ${notes.length} 条`;
  } else {
    ui.historyCount.textContent = hasMoreNotes ? `已显示 ${notes.length} 条` : `共 ${notes.length} 条`;
  }

  // 先用上一轮结果铺一遍，再异步取最新状态，避免日期行闪烁空白
  paintFileStates();
  void refreshFileStates();
}

/**
 * 本地过滤：`#标签` 精确匹配；列表里根本没有这个标签时退回子串匹配，
 * 否则 `：#项目A` 这种「看着像标签、其实不是」的原文会搜不到。
 */
function matchesLocally(note: NoteEntry, term: string): boolean {
  const tag = exactTagTerm(term);
  if (tag === null) return matchesTerm(note, term);
  const known = notes.some((item) =>
    item.tags.some((value) => value.toLowerCase() === tag.toLowerCase()),
  );
  return known
    ? note.tags.some((value) => value.toLowerCase() === tag.toLowerCase())
    : matchesLiteralTag(note, term);
}

/** 点标签即筛选；再点一次（搜索框已经是这个标签）取消筛选。 */
function filterByTag(tag: string): void {
  if (hasOpenEditor()) {
    toast("请先保存或取消正在编辑的笔记", true);
    return;
  }
  if (searchTimer !== undefined) {
    window.clearTimeout(searchTimer);
    searchTimer = undefined;
  }
  ui.historySearch.value = exactTagTerm(ui.historySearch.value) === tag ? "" : `#${tag}`;
  void runSearch();
}

async function loadNotes(reset = true): Promise<void> {
  if (hasOpenEditor()) return;
  const seq = ++searchSeq;
  if (reset) noteOffset = 0;

  try {
    const batch = await invoke<NoteEntry[]>("list_notes", {
      limit: PAGE_SIZE,
      offset: noteOffset,
    });
    if (seq !== searchSeq) return;
    historyError = null;
    // 待删除的条目还没写盘，服务端仍会返回它；只过滤渲染，不影响偏移推进
    const visible = dropPendingDelete(batch);
    notes = reset ? visible : [...notes, ...visible];
    noteOffset = nextOffset(noteOffset, batch.length, reset);
    hasMoreNotes = batch.length === PAGE_SIZE;
    serverFiltered = false;
  } catch (error) {
    if (seq !== searchSeq) return;
    if (reset) notes = [];
    hasMoreNotes = false;
    historyError = String(error);
    toast(String(error), true);
  }
  renderNotes();
  if (reset) ui.entryList.scrollTop = 0;
}

async function runSearch(): Promise<void> {
  if (hasOpenEditor()) {
    toast("请先保存或取消正在编辑的笔记", true);
    return;
  }
  // 待删除项不提前落盘（会缩短撤销窗口），改为在结果里过滤掉
  const seq = ++searchSeq;
  const term = ui.historySearch.value.trim();
  if (!term) {
    serverFiltered = false;
    await loadNotes(true);
    return;
  }

  try {
    const results = await invoke<NoteEntry[]>("search_notes", {
      query: term,
      limit: SEARCH_LIMIT,
    });
    if (seq !== searchSeq) return;
    historyError = null;
    notes = dropPendingDelete(results);
    noteOffset = 0;
    hasMoreNotes = false;
    serverFiltered = true;
  } catch (error) {
    if (seq !== searchSeq) return;
    notes = [];
    serverFiltered = true;
    historyError = String(error);
    toast(String(error), true);
  }
  renderNotes();
  ui.entryList.scrollTop = 0;
}

export function refreshNotes(): void {
  if (hasOpenEditor()) return;
  if (ui.historySearch.value.trim()) {
    void runSearch();
  } else {
    void loadNotes(true);
  }
}

async function toggleHistoryPreview(): Promise<void> {
  if (hasOpenEditor()) {
    toast("请先保存或取消正在编辑的笔记", true);
    return;
  }
  const next = !settings.historyPreview;
  settings.historyPreview = next;
  paintPreviewButton();
  renderNotes();
  try {
    await invoke("update_history_preview", { enabled: next });
  } catch (error) {
    settings.historyPreview = !next;
    paintPreviewButton();
    renderNotes();
    toast(String(error), true);
  }
}

/** 重置历史面板状态并加载第一页；关闭其他面板由 main 负责。 */
export function showHistoryPanel(): void {
  // 编辑框里还有未保存的改动：不能再重建列表，否则输入被静默丢掉。
  if (hasOpenEditor()) {
    showPanel(ui.historyPanel);
    ui.historySearch.focus();
    return;
  }
  showPanel(ui.historyPanel);
  ui.historySearch.value = "";
  notes = [];
  serverFiltered = false;
  noteOffset = 0;
  hasMoreNotes = false;
  historyError = null;
  fileStates = new Map();
  paintPreviewButton();
  ui.entryList.replaceChildren();
  ui.historyCount.textContent = "";
  ui.historySearch.focus();
  void loadNotes(true);
}

/** 模块加载时绑定历史面板内部控件（顶栏 notes-btn 由 main 绑定）。 */
export function initHistory(hooks: HistoryHooks): void {
  historyHooks = hooks;
  ui.historyClose.addEventListener("click", () => closeHistory());
  ui.historyPreview.addEventListener("click", () => void toggleHistoryPreview());
  ui.historySearch.addEventListener("input", () => {
    if (searchTimer !== undefined) window.clearTimeout(searchTimer);
    searchTimer = window.setTimeout(() => {
      searchTimer = undefined;
      void runSearch();
    }, 250);
  });
  ui.historyFolder.addEventListener("click", () => {
    void invoke("open_notes_dir").catch((error) => toast(String(error), true));
  });
  ui.undoAction.addEventListener("click", () => undoDelete());
  // 一轮同步结束后，日期行的状态要跟着变（待同步 → 已同步）
  subscribeSyncStatus(() => {
    if (ui.historyPanel.classList.contains("hidden")) return;
    void refreshFileStates();
  });
}
