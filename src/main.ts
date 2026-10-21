import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";
import { disable, enable, isEnabled } from "@tauri-apps/plugin-autostart";

interface Settings {
  notesDir: string;
  hotkey: string;
  pinned: boolean;
  captureSource: boolean;
  markdownMode: boolean;
  window: unknown;
}

interface SavedNote {
  path: string;
  time: string;
}

interface ShowPayload {
  source: string | null;
}

interface NoteEntry {
  date: string;
  time: string;
  content: string;
  source: string | null;
  path: string;
}

const DRAFT_KEY = "flashnote.draft";

function el<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) throw new Error(`missing element #${id}`);
  return node as T;
}

const textarea = el<HTMLTextAreaElement>("note");
const pinButton = el<HTMLButtonElement>("pin");
const settingsButton = el<HTMLButtonElement>("settings-btn");
const closeButton = el<HTMLButtonElement>("close-btn");
const notesButton = el<HTMLButtonElement>("notes-btn");
const historyPanel = el<HTMLElement>("history");
const historyClose = el<HTMLButtonElement>("history-close");
const historySearch = el<HTMLInputElement>("history-search");
const historyCount = el<HTMLSpanElement>("history-count");
const historyFolder = el<HTMLButtonElement>("history-folder");
const entryList = el<HTMLDivElement>("entry-list");
const settingsPanel = el<HTMLElement>("settings");
const settingsClose = el<HTMLButtonElement>("settings-close");
const hotkeyInput = el<HTMLInputElement>("hotkey-input");
const hotkeyRecord = el<HTMLButtonElement>("hotkey-record");
const hotkeyError = el<HTMLParagraphElement>("hotkey-error");
const notesDirInput = el<HTMLInputElement>("notes-dir");
const chooseDirButton = el<HTMLButtonElement>("choose-dir");
const captureSourceCheck = el<HTMLInputElement>("capture-source");
const markdownModeCheck = el<HTMLInputElement>("markdown-mode");
const autostartCheck = el<HTMLInputElement>("autostart");
const sourceLabel = el<HTMLSpanElement>("source");
const toastEl = el<HTMLDivElement>("toast");
const resizeGrip = el<HTMLDivElement>("resize-grip");

let settings: Settings = {
  notesDir: "",
  hotkey: "ctrl+alt+n",
  pinned: false,
  captureSource: true,
  markdownMode: false,
  window: null,
};
let pinned = false;
let recording = false;
let toastTimer: number | undefined;
let notes: NoteEntry[] = [];

function formatAccel(accel: string): string {
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

function accelFromEvent(event: KeyboardEvent): string | null {
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

function toast(message: string, error = false): void {
  toastEl.textContent = message;
  toastEl.classList.toggle("error", error);
  toastEl.classList.add("show");
  if (toastTimer !== undefined) window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => toastEl.classList.remove("show"), 1600);
}

function paintPin(): void {
  pinButton.classList.toggle("active", pinned);
  pinButton.title = pinned
    ? "常驻模式：保存后保持打开（点击切换）"
    : "临时模式：保存后自动收起（点击切换）";
}

function setSource(source: string | null): void {
  sourceLabel.textContent = source ? `来源：${source}` : "";
  sourceLabel.title = source ?? "";
}

async function refreshSettings(): Promise<void> {
  settings = await invoke<Settings>("get_settings");
  hotkeyInput.value = formatAccel(settings.hotkey);
  notesDirInput.value = settings.notesDir;
  captureSourceCheck.checked = settings.captureSource;
  markdownModeCheck.checked = settings.markdownMode;
}

async function hideWindow(): Promise<void> {
  closeSettings();
  closeHistory();
  await invoke("hide_window");
}

async function saveNote(): Promise<void> {
  const content = textarea.value.trim();
  if (!content) {
    await hideWindow();
    return;
  }

  try {
    const saved = await invoke<SavedNote>("save_note", { content });
    textarea.value = "";
    localStorage.removeItem(DRAFT_KEY);
    toast(`已保存 · ${saved.time}`);
    if (!pinned) {
      window.setTimeout(() => {
        void invoke("hide_window");
      }, 380);
    }
  } catch (error) {
    toast(String(error), true);
  }
}

function openSettings(): void {
  closeHistory();
  hotkeyInput.value = formatAccel(settings.hotkey);
  notesDirInput.value = settings.notesDir;
  captureSourceCheck.checked = settings.captureSource;
  markdownModeCheck.checked = settings.markdownMode;
  hotkeyError.textContent = "";
  settingsPanel.classList.remove("hidden");
}

function stopRecording(): void {
  recording = false;
  hotkeyRecord.classList.remove("recording");
  hotkeyRecord.textContent = "录制";
}

function closeSettings(): void {
  settingsPanel.classList.add("hidden");
  stopRecording();
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

function closeHistory(): void {
  historyPanel.classList.add("hidden");
}

function renderNotes(): void {
  const term = historySearch.value.trim().toLowerCase();
  const filtered = term
    ? notes.filter((note) =>
        [note.date, note.time, note.content, note.source ?? ""].some((value) =>
          value.toLowerCase().includes(term),
        ),
      )
    : notes;

  entryList.replaceChildren();

  if (filtered.length === 0) {
    const empty = document.createElement("p");
    empty.className = "history-empty";
    empty.textContent = notes.length === 0 ? "还没有记录" : "没有匹配的笔记";
    entryList.append(empty);
  } else {
    let lastDate = "";
    for (const note of filtered) {
      if (note.date !== lastDate) {
        lastDate = note.date;
        const date = document.createElement("div");
        date.className = "entry-date";
        date.textContent = dateLabel(note.date);
        entryList.append(date);
      }

      const entry = document.createElement("article");
      entry.className = "entry";

      const head = document.createElement("div");
      head.className = "entry-head";

      const time = document.createElement("span");
      time.className = "entry-time";
      time.textContent = note.time;
      head.append(time);

      if (note.source) {
        const source = document.createElement("span");
        source.className = "entry-source";
        source.textContent = `来源：${note.source}`;
        head.append(source);
      }

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
      head.append(openButton);

      const content = document.createElement("div");
      content.className = "entry-content";
      content.textContent = note.content;

      entry.append(head, content);
      entryList.append(entry);
    }
  }

  historyCount.textContent = term
    ? `${filtered.length} / ${notes.length} 条`
    : `共 ${notes.length} 条`;
}

async function loadNotes(): Promise<void> {
  try {
    notes = await invoke<NoteEntry[]>("list_notes", { limit: 200 });
  } catch (error) {
    notes = [];
    toast(String(error), true);
  }
  renderNotes();
}

function openHistory(): void {
  closeSettings();
  historyPanel.classList.remove("hidden");
  historySearch.value = "";
  notes = [];
  entryList.replaceChildren();
  historyCount.textContent = "";
  historySearch.focus();
  void loadNotes();
}

function startRecording(): void {
  recording = true;
  hotkeyRecord.classList.add("recording");
  hotkeyRecord.textContent = "按键…";
  hotkeyError.textContent = "";
}

async function applyHotkey(accel: string): Promise<void> {
  try {
    await invoke("update_hotkey", { accel });
    await refreshSettings();
    stopRecording();
    toast("快捷键已更新");
  } catch (error) {
    hotkeyError.textContent = String(error);
    stopRecording();
  }
}

function handleRecording(event: KeyboardEvent): void {
  event.preventDefault();
  event.stopPropagation();

  if (event.key === "Escape") {
    stopRecording();
    return;
  }

  const accel = accelFromEvent(event);
  if (accel) void applyHotkey(accel);
}

async function init(): Promise<void> {
  try {
    await refreshSettings();
    pinned = settings.pinned;
    paintPin();

    try {
      autostartCheck.checked = await isEnabled();
    } catch {
      autostartCheck.disabled = true;
    }

    textarea.value = localStorage.getItem(DRAFT_KEY) ?? "";
    textarea.focus();
  } catch (error) {
    toast(String(error), true);
  }
}

textarea.addEventListener("input", () => {
  localStorage.setItem(DRAFT_KEY, textarea.value);
});

pinButton.addEventListener("click", async () => {
  pinned = !pinned;
  paintPin();
  try {
    await invoke("set_pinned", { pinned });
    toast(pinned ? "常驻模式：保存后保持打开" : "保存后自动收起");
  } catch (error) {
    toast(String(error), true);
  }
});

settingsButton.addEventListener("click", openSettings);
settingsClose.addEventListener("click", closeSettings);
notesButton.addEventListener("click", openHistory);
historyClose.addEventListener("click", closeHistory);
historySearch.addEventListener("input", renderNotes);
historyFolder.addEventListener("click", () => {
  void invoke("open_notes_dir").catch((error) => toast(String(error), true));
});
closeButton.addEventListener("click", () => void hideWindow());

hotkeyRecord.addEventListener("click", () => {
  if (recording) {
    stopRecording();
  } else {
    startRecording();
  }
});

chooseDirButton.addEventListener("click", async () => {
  try {
    const selected = await open({
      directory: true,
      multiple: false,
      title: "选择笔记目录",
    });
    if (typeof selected === "string") {
      await invoke("update_notes_dir", { path: selected });
      await refreshSettings();
      toast("笔记目录已更新");
    }
  } catch (error) {
    toast(String(error), true);
  }
});

captureSourceCheck.addEventListener("change", async () => {
  try {
    await invoke("update_capture_source", {
      enabled: captureSourceCheck.checked,
    });
    await refreshSettings();
  } catch (error) {
    captureSourceCheck.checked = !captureSourceCheck.checked;
    toast(String(error), true);
  }
});

markdownModeCheck.addEventListener("change", async () => {
  try {
    await invoke("update_markdown_mode", {
      enabled: markdownModeCheck.checked,
    });
    await refreshSettings();
    toast(markdownModeCheck.checked ? "已开启 Markdown 模式" : "已关闭 Markdown 模式");
  } catch (error) {
    markdownModeCheck.checked = !markdownModeCheck.checked;
    toast(String(error), true);
  }
});

autostartCheck.addEventListener("change", async () => {
  try {
    if (autostartCheck.checked) {
      await enable();
    } else {
      await disable();
    }
    toast(autostartCheck.checked ? "已开启开机自启" : "已关闭开机自启");
  } catch (error) {
    autostartCheck.checked = !autostartCheck.checked;
    toast(String(error), true);
  }
});

window.addEventListener(
  "keydown",
  (event) => {
    if (recording) {
      handleRecording(event);
      return;
    }

    if (event.key === "Escape") {
      event.preventDefault();
      if (!historyPanel.classList.contains("hidden")) {
        closeHistory();
      } else if (!settingsPanel.classList.contains("hidden")) {
        closeSettings();
      } else {
        void hideWindow();
      }
      return;
    }

    if ((event.ctrlKey || event.metaKey) && event.key === "Enter" && !event.isComposing) {
      event.preventDefault();
      void saveNote();
      return;
    }

    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
      event.preventDefault();
      void saveNote();
    }
  },
  true,
);

resizeGrip.addEventListener("pointerdown", (event) => {
  event.preventDefault();
  void getCurrentWindow().startResizeDragging("SouthEast");
});

void listen<ShowPayload>("window-shown", (event) => {
  setSource(event.payload.source);
  if (!historyPanel.classList.contains("hidden")) {
    void loadNotes();
    return;
  }
  if (!settingsPanel.classList.contains("hidden")) return;
  textarea.focus();
});

void listen("open-settings", () => {
  openSettings();
});

void listen("open-history", () => {
  openHistory();
});

void init();
