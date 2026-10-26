use chrono::{DateTime, Local, NaiveDate};
use serde::Serialize;
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::SystemTime,
};

/// 来源标题来自前台窗口，属于外部不可信数据：写入 Markdown 前压平空白、限长，
/// 并消除会破坏斜体包裹或 HTML 注释的序列。
fn sanitize_source(source: Option<&str>) -> Option<String> {
    let flat = source.map(|value| value.split_whitespace().collect::<Vec<_>>().join(" "))?;
    if flat.is_empty() {
        return None;
    }

    let limited: String = flat.chars().take(200).collect();
    let mut out = String::with_capacity(limited.len());
    let mut chars = limited.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\\' => out.push_str("\\\\"),
            '_' => out.push_str("\\_"),
            '*' => out.push_str("\\*"),
            '`' => out.push_str("\\`"),
            '-' if chars.peek() == Some(&'-') => {
                // HTML 注释内不能出现连续的 `--`
                out.push_str("- -");
                chars.next();
            }
            _ => out.push(ch),
        }
    }
    Some(out)
}

fn decode_entities(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        if let Some(after) = tail.strip_prefix("&amp;") {
            out.push('&');
            rest = after;
        } else if let Some(after) = tail.strip_prefix("&lt;") {
            out.push('<');
            rest = after;
        } else if let Some(after) = tail.strip_prefix("&gt;") {
            out.push('>');
            rest = after;
        } else {
            out.push('&');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

/// `sanitize_source` 的逆操作，保证来源字段读写 round-trip 一致。
fn decode_source(raw: &str) -> String {
    let normalized = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::with_capacity(normalized.len());
    let mut chars = normalized.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.peek().copied() {
                Some(next) if matches!(next, '\\' | '_' | '*' | '`') => {
                    out.push(next);
                    chars.next();
                    continue;
                }
                _ => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    decode_entities(&out).replace("- -", "--")
}

/// 把一条速记渲染成 Markdown 列表项。
/// 多行内容缩进两格，来源作为斜体续行。
pub fn format_entry(time: &str, content: &str, source: Option<&str>) -> String {
    let trimmed = content.trim();
    let mut lines = trimmed.lines();
    let first = lines.next().unwrap_or("");
    let mut out = format!("- {time} {first}");

    for line in lines {
        out.push('\n');
        if !line.is_empty() {
            out.push_str("  ");
            out.push_str(line);
        }
    }

    if let Some(source) = sanitize_source(source) {
        out.push_str("\n  _来源：");
        out.push_str(&source);
        out.push('_');
    }

    out
}

/// Markdown 模式：时间作为三级标题，原文不做缩进，来源写成 HTML 注释。
pub fn format_entry_markdown(time: &str, content: &str, source: Option<&str>) -> String {
    let mut out = format!("### {time}\n{}", content.trim());

    if let Some(source) = sanitize_source(source) {
        out.push_str("\n\n<!-- 来源：");
        out.push_str(&source);
        out.push_str(" -->");
    }

    out
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteEntry {
    pub date: String,
    pub time: String,
    pub content: String,
    pub source: Option<String>,
    pub path: String,
}

/// 文件中的一个原始条目块，保留原始行，便于删除 / 编辑后重建。
pub(crate) struct RawEntry {
    pub time: String,
    pub lines: Vec<String>,
}

/// 文件里的一个条目块，以及它在原文中的行区间。
///
/// `start_line` 指向条目首行，`end_line` 是块内最后一个非空行的下一行——块尾的空行
/// 属于块与块之间的分隔，不计入区间，这样删块时不会连带吞掉相邻块的空行。
pub(crate) struct BlockSpan {
    // 行号目前只有同步合并在用；引擎接线前先让死代码检查通过。
    #[allow(dead_code)]
    pub start_line: usize,
    #[allow(dead_code)]
    pub end_line: usize,
    pub entry: RawEntry,
}

/// `split_spans` 的结果：头部与带行号的条目块。
pub(crate) struct NoteLayout {
    pub header: Vec<String>,
    pub spans: Vec<BlockSpan>,
}

/// 把文件拆成「头部 + 条目块」，并记录每个块在原文中的行号。
/// 代码围栏内的标题行不会被当作新条目。
pub(crate) fn split_spans(text: &str) -> NoteLayout {
    struct Pending {
        start: usize,
        last_content: usize,
        entry: RawEntry,
    }

    let mut header = Vec::new();
    let mut spans: Vec<BlockSpan> = Vec::new();
    let mut current: Option<Pending> = None;
    let mut open_fence: Option<Fence> = None;
    let mut previous_blank = true;

    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim_end_matches('\r').to_string();
        let is_blank = line.trim().is_empty();

        if let Some(open) = open_fence {
            if let Some(pending) = current.as_mut() {
                pending.entry.lines.push(line.clone());
                if !is_blank {
                    pending.last_content = index;
                }
            }
            if closes_fence(&line, open) {
                open_fence = None;
            }
            previous_blank = is_blank;
            continue;
        }

        let current_is_markdown = current
            .as_ref()
            .and_then(|pending| pending.entry.lines.first())
            .is_some_and(|line| markdown_time(line).is_some());

        if let Some((time, head)) = entry_header(&line, current_is_markdown, previous_blank) {
            if let Some(done) = current.take() {
                spans.push(BlockSpan {
                    start_line: done.start,
                    end_line: done.last_content + 1,
                    entry: done.entry,
                });
            }
            // 紧凑列表的围栏可能直接写在条目头里：`- 10:00 ```js`
            open_fence = fence_marker(&head);
            current = Some(Pending {
                start: index,
                last_content: index,
                entry: RawEntry {
                    time,
                    lines: vec![line],
                },
            });
            previous_blank = is_blank;
            continue;
        }

        match current.as_mut() {
            Some(pending) => {
                open_fence = fence_marker(&line);
                pending.entry.lines.push(line);
                if !is_blank {
                    pending.last_content = index;
                }
            }
            None => header.push(line),
        }
        previous_blank = is_blank;
    }

    if let Some(done) = current {
        spans.push(BlockSpan {
            start_line: done.start,
            end_line: done.last_content + 1,
            entry: done.entry,
        });
    }

    for span in &mut spans {
        while span.entry.lines.last().is_some_and(|line| line.is_empty()) {
            span.entry.lines.pop();
        }
    }
    while header.last().is_some_and(|line| line.is_empty()) {
        header.pop();
    }

    NoteLayout { header, spans }
}

/// 把文件拆成「头部 + 条目块」。代码围栏内的标题行不会被当作新条目。
pub(crate) fn split_entries(text: &str) -> (Vec<String>, Vec<RawEntry>) {
    let layout = split_spans(text);
    (
        layout.header,
        layout.spans.into_iter().map(|span| span.entry).collect(),
    )
}

fn content_line(line: &str, compact: bool) -> String {
    if compact {
        line.strip_prefix("  ").unwrap_or(line).to_string()
    } else {
        line.to_string()
    }
}

pub(crate) fn parse_block(block: &RawEntry) -> Option<(String, String, Option<String>)> {
    let first = block.lines.first()?;
    let mut content: Vec<String> = Vec::new();
    let mut source = None;

    let (compact, first_head) = if let Some((_, head)) = list_time(first) {
        (true, head)
    } else if markdown_time(first).is_some() {
        (false, String::new())
    } else {
        return None;
    };

    if compact && !first_head.is_empty() {
        content.push(first_head.clone());
    }
    let body = &block.lines[1..];
    // 先标出哪些行在代码围栏内：围栏里的来源行只是正文。
    let mut fenced = Vec::with_capacity(body.len());
    let mut open_fence = fence_marker(&first_head);
    for line in body {
        if let Some(open) = open_fence {
            fenced.push(true);
            if closes_fence(line, open) {
                open_fence = None;
            }
        } else {
            fenced.push(false);
            open_fence = fence_marker(line);
        }
    }
    // 只有块尾最后一个非空行才是来源。正文中间形似 `_来源：…_` 的行必须原样保留，
    // 否则用户写到「来源」二字（或粘贴类似片段）时，整行会被吞掉并改写语义。
    let source_index = body
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .filter(|index| !fenced[*index])
        .filter(|index| source_line(&body[*index]).is_some());

    for (index, line) in body.iter().enumerate() {
        if Some(index) == source_index {
            source = source_line(line);
            continue;
        }
        // 围栏内的内容原样保留，`<!-- 来源：… -->` 只是正文
        content.push(content_line(line, compact));
    }

    let content = content.join("\n").trim_matches('\n').trim_end().to_string();
    if content.is_empty() {
        return None;
    }
    Some((block.time.clone(), content, source))
}

fn is_hhmm(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 5
        && bytes[0].is_ascii_digit()
        && bytes[1].is_ascii_digit()
        && bytes[2] == b':'
        && bytes[3].is_ascii_digit()
        && bytes[4].is_ascii_digit()
}

fn markdown_time(line: &str) -> Option<String> {
    let value = line.trim_start().strip_prefix("### ")?.trim();
    is_hhmm(value).then(|| value.to_string())
}

fn list_time(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("- ")?;
    let (time, first) = rest.split_once(' ').unwrap_or((rest, ""));
    is_hhmm(time).then(|| (time.to_string(), first.to_string()))
}

fn source_line(line: &str) -> Option<String> {
    let value = line.trim();
    let raw = value
        .strip_prefix("_来源：")
        .and_then(|rest| rest.strip_suffix('_'))
        .or_else(|| {
            value
                .strip_prefix("<!-- 来源：")
                .and_then(|rest| rest.strip_suffix("-->"))
        })?;
    let decoded = decode_source(raw);
    (!decoded.is_empty()).then_some(decoded)
}

/// 围栏的开标记：字符（` 或 ~）与长度。闭合标记必须同字符且不短于它。
#[derive(Clone, Copy)]
struct Fence {
    marker: char,
    len: usize,
}

fn entry_header(
    line: &str,
    current_is_markdown: bool,
    previous_blank: bool,
) -> Option<(String, String)> {
    // 只有第 0 列的 `### HH:MM` 才算条目头。紧凑模式的正文行会缩进两格，
    // 正文里恰好写成 `### 10:00` 时必须保持为正文，否则整条笔记会被拆开。
    if !line.starts_with(char::is_whitespace) {
        if let Some(time) = markdown_time(line) {
            // Markdown 条目的正文从下一行开始，标题行本身不带内容。
            return Some((time, String::new()));
        }
    }
    // Markdown 块正文里的 `- HH:MM` 是普通内容；只有空行之后的列表行才视为新条目（兼容混用格式）。
    if current_is_markdown && !previous_blank {
        return None;
    }
    list_time(line)
}

fn fence_marker(line: &str) -> Option<Fence> {
    let trimmed = line.trim_start();
    let marker = trimmed.chars().next()?;
    if marker != '`' && marker != '~' {
        return None;
    }
    let len = trimmed.chars().take_while(|ch| *ch == marker).count();
    (len >= 3).then_some(Fence { marker, len })
}

fn closes_fence(line: &str, open: Fence) -> bool {
    let trimmed = line.trim_start();
    let len = trimmed.chars().take_while(|ch| *ch == open.marker).count();
    len >= open.len && trimmed.chars().skip(len).all(|ch| ch.is_whitespace())
}

/// 解析单个日期文件，兼容紧凑列表与 Markdown 两种格式。
pub fn parse_entries(text: &str) -> Vec<(String, String, Option<String>)> {
    split_entries(text)
        .1
        .iter()
        .filter_map(parse_block)
        .collect()
}

/// 用头部与条目块重建整个文件。
pub(crate) fn render_file(date: &str, header: &[String], blocks: &[RawEntry]) -> String {
    let head = if header.is_empty() {
        format!("# {date}")
    } else {
        header.join("\n")
    };
    if blocks.is_empty() {
        return format!("{head}\n");
    }
    let body = blocks
        .iter()
        .map(|block| block.lines.join("\n"))
        .collect::<Vec<_>>()
        .join("\n\n");
    format!("{head}\n\n{body}\n")
}

fn find_block(blocks: &[RawEntry], time: &str, content: &str) -> Option<usize> {
    blocks.iter().position(|block| {
        block.time == time && parse_block(block).is_some_and(|(_, value, _)| value == content)
    })
}

/// 删除一条笔记；找不到时返回 `None`，调用方不应改写文件。
pub fn remove_entry(text: &str, date: &str, time: &str, content: &str) -> Option<String> {
    let (header, mut blocks) = split_entries(text);
    let index = find_block(&blocks, time, content)?;
    blocks.remove(index);
    Some(render_file(date, &header, &blocks))
}

/// 原地替换一条笔记的内容，保留原有格式与来源。
pub fn replace_entry(
    text: &str,
    date: &str,
    time: &str,
    content: &str,
    new_content: &str,
) -> Option<String> {
    let (header, mut blocks) = split_entries(text);
    let index = find_block(&blocks, time, content)?;
    let markdown = markdown_time(&blocks[index].lines[0]).is_some();
    let source = parse_block(&blocks[index]).and_then(|(_, _, source)| source);
    let rendered = if markdown {
        format_entry_markdown(time, new_content, source.as_deref())
    } else {
        format_entry(time, new_content, source.as_deref())
    };
    blocks[index] = RawEntry {
        time: time.to_string(),
        lines: rendered.lines().map(str::to_string).collect(),
    };
    Some(render_file(date, &header, &blocks))
}

/// 列出笔记目录中的日期文件，按日期倒序。
fn note_files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(|item| item.ok())
        .map(|item| item.path())
        .filter(|path| {
            path.extension().and_then(|ext| ext.to_str()) == Some("md")
                && path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .and_then(|stem| NaiveDate::parse_from_str(stem, "%Y-%m-%d").ok())
                    .is_some()
        })
        .collect();
    files.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    Ok(files)
}

fn entry_for(
    path: &Path,
    date: &str,
    time: String,
    content: String,
    source: Option<String>,
) -> NoteEntry {
    NoteEntry {
        date: date.to_string(),
        time,
        content,
        source,
        path: path.to_string_lossy().to_string(),
    }
}

/// 笔记文件可能是非 UTF-8（例如被 ANSI 编辑器改写过）：有损解码，保证内容仍可见。
fn read_note(path: &Path) -> std::io::Result<String> {
    let bytes = fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// 解析结果的缓存项，用 (mtime, size) 判断文件是否被外部改动过。
struct CachedFile {
    modified: SystemTime,
    len: u64,
    entries: Vec<(String, String, Option<String>)>,
}

/// 缓存条目上限：长期运行且频繁换目录时不至于无限增长。
const CACHE_LIMIT: usize = 1024;

fn parse_cache() -> &'static Mutex<HashMap<PathBuf, CachedFile>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedFile>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_cache<T>(action: impl FnOnce(&mut HashMap<PathBuf, CachedFile>) -> T) -> T {
    let mut cache = parse_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    action(&mut cache)
}

/// 我们自己改写文件后立刻让缓存失效，不依赖文件系统时间戳的精度。
pub fn invalidate_cache(path: &Path) {
    with_cache(|cache| cache.remove(path));
}

/// 清空整个解析缓存。改写路径拿到的可能是一份 canonicalize 过的路径，和
/// 缓存键（`notes_dir.join(name)`）不一定逐字相等，直接全清最稳妥。
pub(crate) fn invalidate_all() {
    with_cache(|cache| cache.clear());
}

/// 读取并解析一个日期文件；命中缓存时只需一次 stat，不用读盘和重新解析。
fn entries_of(path: &Path) -> Option<Vec<(String, String, Option<String>)>> {
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(error) => {
            log::warn!("读取笔记文件失败 {}：{error}", path.display());
            return None;
        }
    };
    let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let len = meta.len();

    let cached = with_cache(|cache| {
        cache
            .get(path)
            .filter(|hit| hit.modified == modified && hit.len == len)
            .map(|hit| hit.entries.clone())
    });
    if let Some(entries) = cached {
        return Some(entries);
    }

    let text = match read_note(path) {
        Ok(text) => text,
        Err(error) => {
            log::warn!("读取笔记文件失败 {}：{error}", path.display());
            return None;
        }
    };
    let entries = parse_entries(&text);
    with_cache(|cache| {
        if cache.len() >= CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(
            path.to_path_buf(),
            CachedFile {
                modified,
                len,
                entries: entries.clone(),
            },
        );
    });
    Some(entries)
}

/// 不区分 ASCII 大小写的子串匹配，不产生临时字符串。
fn contains_fold(haystack: &str, needle: &str) -> bool {
    let hay = haystack.as_bytes();
    let pat = needle.as_bytes();
    pat.is_empty()
        || (pat.len() <= hay.len() && hay.windows(pat.len()).any(|w| w.eq_ignore_ascii_case(pat)))
}

/// 一行笔记是否命中搜索词：日期、时间、正文、来源分开匹配，免去拼接与整体小写化。
fn matches_query(
    date: &str,
    time: &str,
    content: &str,
    source: Option<&str>,
    needle: &str,
    folded: &str,
) -> bool {
    let source = source.unwrap_or("");
    let fields = [date, time, content, source];
    if needle.is_ascii() {
        fields.iter().any(|field| contains_fold(field, needle))
    } else {
        // 非 ASCII 的匹配只能走完整小写化，这种查询很少见
        fields
            .iter()
            .any(|field| field.to_lowercase().contains(folded))
    }
}

/// 读取笔记，按日期与写入顺序倒序返回；`offset` 用于分页。
pub fn load_recent(dir: &Path, limit: usize, offset: usize) -> std::io::Result<Vec<NoteEntry>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut skipped = 0usize;
    let mut result = Vec::new();
    for path in note_files(dir)? {
        let date = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("")
            .to_string();
        let Some(mut parsed) = entries_of(&path) else {
            continue;
        };
        parsed.reverse();
        for (time, content, source) in parsed {
            if skipped < offset {
                skipped += 1;
                continue;
            }
            result.push(entry_for(&path, &date, time, content, source));
            if result.len() >= limit {
                return Ok(result);
            }
        }
    }
    Ok(result)
}

/// 全文搜索（不区分大小写），按日期与写入顺序倒序返回，最多 `limit` 条。
pub fn search_entries(dir: &Path, query: &str, limit: usize) -> std::io::Result<Vec<NoteEntry>> {
    let needle = query.trim();
    if needle.is_empty() || !dir.exists() {
        return Ok(Vec::new());
    }
    let folded = needle.to_lowercase();

    let mut result = Vec::new();
    for path in note_files(dir)? {
        let date = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("")
            .to_string();
        let Some(mut parsed) = entries_of(&path) else {
            continue;
        };
        parsed.reverse();
        for (time, content, source) in parsed {
            if matches_query(&date, &time, &content, source.as_deref(), needle, &folded) {
                result.push(entry_for(&path, &date, time, content, source));
                if result.len() >= limit {
                    return Ok(result);
                }
            }
        }
    }
    Ok(result)
}

/// 追加到 <dir>/<date>.md。文件不存在时写入日期标题。
pub fn append_note(dir: &Path, date: &str, entry: &str) -> std::io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("{date}.md"));
    // 只有文件确实不存在时才按空文件处理；读取失败必须上抛，否则会重复写入日期标题。
    let existing = match read_note(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;

    if existing.is_empty() {
        write!(file, "# {date}\n\n")?;
    } else if !existing.ends_with("\n\n") {
        writeln!(file)?;
    }
    writeln!(file, "{entry}")?;
    // 追加后 mtime 会变，但同一秒内的多次写入仍可能撞上旧缓存，这里直接清掉
    drop(file);
    invalidate_cache(&path);

    Ok(path)
}

pub fn now_parts() -> (String, String) {
    let now: DateTime<Local> = Local::now();
    (
        now.format("%Y-%m-%d").to_string(),
        now.format("%H:%M").to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_single_line() {
        assert_eq!(format_entry("14:32", "一个想法", None), "- 14:32 一个想法");
    }

    #[test]
    fn formats_multiline_and_source() {
        assert_eq!(
            format_entry("14:32", "第一行\n第二行", Some("Chrome — 文章")),
            "- 14:32 第一行\n  第二行\n  _来源：Chrome — 文章_"
        );
    }

    #[test]
    fn formats_markdown_block_with_source() {
        assert_eq!(
            format_entry_markdown(
                "14:32",
                "## 小标题\n\n```js\nconst a = 1;\n```",
                Some("Chrome — 文章")
            ),
            "### 14:32\n## 小标题\n\n```js\nconst a = 1;\n```\n\n<!-- 来源：Chrome — 文章 -->"
        );
    }

    #[test]
    fn formats_markdown_block_without_source() {
        assert_eq!(
            format_entry_markdown("09:05", "  第一行\n第二行  ", None),
            "### 09:05\n第一行\n第二行"
        );
    }

    #[test]
    fn split_spans_tracks_line_ranges() {
        let text = "# 2026-10-23\n\n- 10:00 a\n  b\n\n\n- 11:00 c\n";
        let layout = split_spans(text);
        assert_eq!(layout.header, vec!["# 2026-10-23"]);
        assert_eq!(layout.spans.len(), 2);
        assert_eq!(layout.spans[0].start_line, 2);
        assert_eq!(layout.spans[0].end_line, 4);
        assert_eq!(layout.spans[1].start_line, 6);
        assert_eq!(layout.spans[1].end_line, 7);
        assert_eq!(layout.spans[1].entry.lines, vec!["- 11:00 c"]);
    }

    #[test]
    fn split_spans_matches_split_entries() {
        let text = "# 2026-10-23\r\n\r\n### 10:00\r\n正文\r\n\r\n- 11:00 b\r\n";
        let layout = split_spans(text);
        let (header, blocks) = split_entries(text);
        assert_eq!(layout.header, header);
        let entries: Vec<_> = layout.spans.into_iter().map(|span| span.entry).collect();
        assert_eq!(entries.len(), blocks.len());
        for (left, right) in entries.iter().zip(blocks.iter()) {
            assert_eq!(left.time, right.time);
            assert_eq!(left.lines, right.lines);
        }
    }

    #[test]
    fn parses_list_entries() {
        let text =
            "# 2026-10-21\n\n- 14:32 first\n  second\n  _来源：Chrome — 文章_\n\n- 15:02 next\n";
        let entries = parse_entries(text);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, "14:32");
        assert_eq!(entries[0].1, "first\nsecond");
        assert_eq!(entries[0].2.as_deref(), Some("Chrome — 文章"));
        assert_eq!(entries[1].1, "next");
    }

    #[test]
    fn parses_markdown_entries() {
        let text = "# 2026-10-21\n\n### 14:32\n## 小标题\n\n```js\nconst a = 1;\n```\n\n<!-- 来源：VS Code — notes.rs -->\n";
        let entries = parse_entries(text);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "14:32");
        assert_eq!(entries[0].1, "## 小标题\n\n```js\nconst a = 1;\n```");
        assert_eq!(entries[0].2.as_deref(), Some("VS Code — notes.rs"));
    }

    #[test]
    fn ignores_entry_like_lines_inside_fences() {
        let text = "### 10:00\n```\n### 11:00\n- 12:00 not an entry\n```\n";
        let entries = parse_entries(text);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1, "```\n### 11:00\n- 12:00 not an entry\n```");
    }

    #[test]
    fn keeps_unterminated_fence_in_one_entry() {
        let text = "### 10:00\n```js\n- 11:00 看起来像条目\n";
        let entries = parse_entries(text);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1, "```js\n- 11:00 看起来像条目");
    }

    #[test]
    fn entry_starting_with_fence_does_not_swallow_following_entries() {
        let text = "# 2026-10-21\n\n- 10:00 ```js\n  const a = 1;\n  ```\n\n- 11:00 b\n";
        let entries = parse_entries(text);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, "10:00");
        assert_eq!(entries[0].1, "```js\nconst a = 1;\n```");
        assert_eq!(entries[1].0, "11:00");
        assert_eq!(entries[1].1, "b");
    }

    #[test]
    fn fence_char_must_match_to_close() {
        let text = "### 10:00\n~~~\n```js\n~~~\n### 11:00\nb\n";
        let entries = parse_entries(text);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].1, "~~~\n```js\n~~~");
        assert_eq!(entries[1].1, "b");
    }

    #[test]
    fn fence_line_with_info_string_does_not_close() {
        let text = "### 10:00\n```js\nx\n```js\n### 11:00\nb\n";
        let entries = parse_entries(text);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1, "```js\nx\n```js\n### 11:00\nb");
    }

    #[test]
    fn markdown_body_list_like_line_is_not_a_new_entry() {
        let text = "### 10:00\n- 14:00 评审\n";
        let entries = parse_entries(text);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1, "- 14:00 评审");
    }

    #[test]
    fn indented_markdown_time_inside_compact_body_stays_content() {
        let text = "- 10:05 上午讨论\n  ### 10:00\n  要点\n";
        let entries = parse_entries(text);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "10:05");
        assert_eq!(entries[0].1, "上午讨论\n### 10:00\n要点");
    }

    #[test]
    fn full_width_indented_markdown_time_stays_content() {
        let text = "- 10:05 上午讨论\n\u{3000}### 10:00\n\u{3000}要点\n";
        let entries = parse_entries(text);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].1.contains("### 10:00"));
    }

    #[test]
    fn list_entry_after_markdown_block_and_blank_line() {
        let text = "### 10:00\n正文\n\n- 14:00 新条目\n";
        let entries = parse_entries(text);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].1, "正文");
        assert_eq!(entries[1].0, "14:00");
        assert_eq!(entries[1].1, "新条目");
    }

    #[test]
    fn source_like_line_in_the_middle_stays_content() {
        let text = "- 10:00 a\n  _来源：会议记录_\n  正文\n";
        let entries = parse_entries(text);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].2, None, "只有块尾的来源行才算来源");
        assert_eq!(entries[0].1, "a\n_来源：会议记录_\n正文");
    }

    #[test]
    fn source_like_line_inside_fence_is_kept_as_content() {
        let text = "### 10:00\n```html\n<!-- 来源：百度 -->\n```\n\n<!-- 来源：VS Code -->\n";
        let entries = parse_entries(text);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1, "```html\n<!-- 来源：百度 -->\n```");
        assert_eq!(entries[0].2.as_deref(), Some("VS Code"));
    }

    #[test]
    fn compact_source_like_line_inside_fence_is_kept_as_content() {
        let text = "- 10:00 ```html\n  <!-- 来源：百度 -->\n  ```\n  _来源：Chrome_\n";
        let entries = parse_entries(text);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1, "```html\n<!-- 来源：百度 -->\n```");
        assert_eq!(entries[0].2.as_deref(), Some("Chrome"));
    }

    #[test]
    fn parses_mixed_formats_in_one_file() {
        let text = "# 2026-10-21\n\n- 10:00 列表条目\n  _来源：Chrome_\n\n### 11:00\n## Markdown 条目\n\n<!-- 来源：VS Code -->\n";
        let entries = parse_entries(text);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, "10:00");
        assert_eq!(entries[0].1, "列表条目");
        assert_eq!(entries[0].2.as_deref(), Some("Chrome"));
        assert_eq!(entries[1].0, "11:00");
        assert_eq!(entries[1].1, "## Markdown 条目");
        assert_eq!(entries[1].2.as_deref(), Some("VS Code"));
    }

    #[test]
    fn empty_file_has_no_entries() {
        assert!(parse_entries("").is_empty());
    }

    #[test]
    fn header_only_file_has_no_entries() {
        assert!(parse_entries("# 2026-10-21\n").is_empty());
    }

    #[test]
    fn entry_with_only_source_is_skipped() {
        let text = "# 2026-10-21\n\n- 10:00\n  _来源：Chrome_\n";
        assert!(parse_entries(text).is_empty());
    }

    #[test]
    fn loads_recent_entries_newest_first() {
        let dir =
            std::env::temp_dir().join(format!("flashnote-test-recent-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);

        append_note(&dir, "2026-10-20", "- 10:00 older").unwrap();
        append_note(&dir, "2026-10-21", "- 09:00 newer").unwrap();

        let entries = load_recent(&dir, 10, 0).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].date, "2026-10-21");
        assert_eq!(entries[0].time, "09:00");
        assert_eq!(entries[1].date, "2026-10-20");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_recent_honors_offset() {
        let dir =
            std::env::temp_dir().join(format!("flashnote-test-offset-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);

        append_note(&dir, "2026-10-21", "- 09:00 a\n\n- 10:00 b\n\n- 11:00 c").unwrap();

        let first = load_recent(&dir, 1, 0).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].time, "11:00");

        let second = load_recent(&dir, 1, 1).unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].time, "10:00");

        assert!(load_recent(&dir, 10, 5).unwrap().is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn removes_entry_and_keeps_others() {
        let text = "# 2026-10-21\n\n- 10:00 a\n\n- 11:00 b\n";
        let updated = remove_entry(text, "2026-10-21", "10:00", "a").unwrap();
        assert_eq!(updated, "# 2026-10-21\n\n- 11:00 b\n");
    }

    #[test]
    fn remove_returns_none_when_entry_missing() {
        let text = "# 2026-10-21\n\n- 10:00 a\n";
        assert!(remove_entry(text, "2026-10-21", "10:00", "other").is_none());
        assert!(remove_entry(text, "2026-10-21", "11:00", "a").is_none());
    }

    #[test]
    fn removes_last_entry_leaving_header() {
        let text = "# 2026-10-21\n\n- 10:00 a\n";
        let updated = remove_entry(text, "2026-10-21", "10:00", "a").unwrap();
        assert_eq!(updated, "# 2026-10-21\n");
    }

    #[test]
    fn replaces_entry_content_in_place() {
        let text = "# 2026-10-21\n\n- 10:00 old\n  _来源：Chrome_\n\n- 11:00 b\n";
        let updated = replace_entry(text, "2026-10-21", "10:00", "old", "new line").unwrap();
        assert_eq!(
            updated,
            "# 2026-10-21\n\n- 10:00 new line\n  _来源：Chrome_\n\n- 11:00 b\n"
        );
    }

    #[test]
    fn replace_keeps_markdown_format() {
        let text = "# 2026-10-21\n\n### 10:00\n## 标题\n\n<!-- 来源：VS Code -->\n";
        let updated = replace_entry(text, "2026-10-21", "10:00", "## 标题", "## 新标题").unwrap();
        assert_eq!(
            updated,
            "# 2026-10-21\n\n### 10:00\n## 新标题\n\n<!-- 来源：VS Code -->\n"
        );
    }

    #[test]
    fn searches_all_files_case_insensitively() {
        let dir =
            std::env::temp_dir().join(format!("flashnote-test-search-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);

        append_note(&dir, "2026-10-20", "- 10:00 Rust 所有权\n  _来源：Chrome_").unwrap();
        append_note(&dir, "2026-10-21", "- 09:00 无关键词").unwrap();

        let hits = search_entries(&dir, "rust", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].date, "2026-10-20");
        assert_eq!(hits[0].content, "Rust 所有权");

        let by_source = search_entries(&dir, "chrome", 10).unwrap();
        assert_eq!(by_source.len(), 1);

        assert!(search_entries(&dir, "  ", 10).unwrap().is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn searches_non_ascii_needles() {
        let dir =
            std::env::temp_dir().join(format!("flashnote-test-search-cjk-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);

        append_note(&dir, "2026-10-20", "- 10:00 记录一下闪记的用法").unwrap();

        let hits = search_entries(&dir, "闪记", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].content, "记录一下闪记的用法");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_cache_sees_external_rewrites() {
        let dir = std::env::temp_dir().join(format!("flashnote-test-cache-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("2026-10-20.md");

        fs::write(&path, "# 2026-10-20\n\n- 10:00 第一次\n").unwrap();
        let first = load_recent(&dir, 10, 0).unwrap();
        assert_eq!(first[0].content, "第一次");

        // 外部程序改写：长度与内容都变了，缓存必须失效
        fs::write(&path, "# 2026-10-20\n\n- 11:00 外部改写的更长的内容\n").unwrap();
        let second = load_recent(&dir, 10, 0).unwrap();
        assert_eq!(second[0].content, "外部改写的更长的内容");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn appending_refreshes_cached_entries() {
        let dir = std::env::temp_dir().join(format!(
            "flashnote-test-cache-append-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);

        append_note(&dir, "2026-10-20", "- 10:00 第一条").unwrap();
        assert_eq!(load_recent(&dir, 10, 0).unwrap().len(), 1);

        append_note(&dir, "2026-10-20", "- 11:00 第二条").unwrap();
        let all = load_recent(&dir, 10, 0).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].content, "第二条");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn normalizes_source_whitespace() {
        assert_eq!(
            format_entry("09:05", "x", Some("  A \n  B  ")),
            "- 09:05 x\n  _来源：A B_"
        );
    }

    #[test]
    fn loads_entries_from_non_utf8_file() {
        let dir =
            std::env::temp_dir().join(format!("flashnote-test-ansi-load-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("2026-10-21.md"),
            b"# 2026-10-21\n\n- 10:00 \xff legacy\n",
        )
        .unwrap();

        let entries = load_recent(&dir, 10, 0).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].content.ends_with("legacy"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_does_not_duplicate_header_for_non_utf8_file() {
        let dir =
            std::env::temp_dir().join(format!("flashnote-test-ansi-append-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("2026-10-21.md");
        fs::write(&path, b"# 2026-10-21\n\n- 10:00 \xff legacy\n").unwrap();

        append_note(&dir, "2026-10-21", "- 11:00 b").unwrap();

        let text = String::from_utf8_lossy(&fs::read(&path).unwrap()).into_owned();
        assert_eq!(text.matches("# 2026-10-21").count(), 1);
        assert!(text.contains("- 11:00 b"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sanitizes_source_in_markdown_mode_and_round_trips() {
        let source = r#"evil --> <img src=x onerror=1> _*` & \ end"#;
        let formatted = format_entry_markdown("10:00", "x", Some(source));
        assert!(!formatted.contains("<img"));
        // 来源体里的 `-->` 必须被打断，不能提前闭合注释（末尾的 `-->` 是正常包装）
        assert!(formatted.contains("- -&gt;"));

        let text = format!("# 2026-10-21\n\n{formatted}\n");
        let entries = parse_entries(&text);
        assert_eq!(entries[0].2.as_deref(), Some(source));
    }

    #[test]
    fn sanitizes_source_in_compact_mode_and_round_trips() {
        let source = "a --> b <c> _d_";
        let formatted = format_entry("10:00", "x", Some(source));
        assert!(!formatted.contains("-->"));

        let text = format!("# 2026-10-21\n\n{formatted}\n");
        let entries = parse_entries(&text);
        assert_eq!(entries[0].2.as_deref(), Some(source));
    }

    #[test]
    fn truncates_long_source_to_200_chars() {
        let source = "a".repeat(300);
        let formatted = format_entry("10:00", "x", Some(&source));
        let text = format!("# 2026-10-21\n\n{formatted}\n");
        let entries = parse_entries(&text);
        assert_eq!(entries[0].2.as_deref().unwrap().chars().count(), 200);
    }

    #[test]
    fn appends_entries_with_blank_line_separator() {
        let dir = std::env::temp_dir().join(format!("flashnote-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);

        let path = append_note(&dir, "2026-10-21", "- 10:00 a").unwrap();
        append_note(&dir, "2026-10-21", "- 11:00 b").unwrap();

        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text, "# 2026-10-21\n\n- 10:00 a\n\n- 11:00 b\n");

        let _ = fs::remove_dir_all(&dir);
    }
}
