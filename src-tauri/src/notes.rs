use chrono::{DateTime, Local, NaiveDate};
use serde::Serialize;
use std::{
    fs::{self, OpenOptions},
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

fn normalized_source(source: Option<&str>) -> Option<String> {
    source
        .map(|value| value.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|value| !value.is_empty())
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

    if let Some(source) = normalized_source(source) {
        out.push_str("\n  _来源：");
        out.push_str(&source);
        out.push('_');
    }

    out
}

/// Markdown 模式：时间作为三级标题，原文不做缩进，来源写成 HTML 注释。
pub fn format_entry_markdown(time: &str, content: &str, source: Option<&str>) -> String {
    let mut out = format!("### {time}\n{}", content.trim());

    if let Some(source) = normalized_source(source) {
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

/// 把文件拆成「头部 + 条目块」。代码围栏内的标题行不会被当作新条目。
pub(crate) fn split_entries(text: &str) -> (Vec<String>, Vec<RawEntry>) {
    let mut header = Vec::new();
    let mut blocks: Vec<RawEntry> = Vec::new();
    let mut current: Option<RawEntry> = None;
    let mut open_fence: Option<Fence> = None;

    for raw in text.lines() {
        let line = raw.trim_end_matches('\r').to_string();

        if let Some(open) = open_fence {
            if let Some(entry) = current.as_mut() {
                entry.lines.push(line.clone());
            }
            if closes_fence(&line, open) {
                open_fence = None;
            }
            continue;
        }

        if let Some((time, head)) = entry_header(&line) {
            if let Some(done) = current.take() {
                blocks.push(done);
            }
            // 紧凑列表的围栏可能直接写在条目头里：`- 10:00 ```js`
            open_fence = fence_marker(&head);
            current = Some(RawEntry {
                time,
                lines: vec![line],
            });
            continue;
        }

        match current.as_mut() {
            Some(entry) => {
                open_fence = fence_marker(&line);
                entry.lines.push(line);
            }
            None => header.push(line),
        }
    }

    if let Some(done) = current {
        blocks.push(done);
    }

    for block in &mut blocks {
        while block.lines.last().is_some_and(|line| line.is_empty()) {
            block.lines.pop();
        }
    }
    while header.last().is_some_and(|line| line.is_empty()) {
        header.pop();
    }

    (header, blocks)
}

fn parse_block(block: &RawEntry) -> Option<(String, String, Option<String>)> {
    let first = block.lines.first()?;
    let mut content: Vec<String> = Vec::new();
    let mut source = None;

    if let Some((_, head)) = list_time(first) {
        if !head.is_empty() {
            content.push(head);
        }
        for line in &block.lines[1..] {
            if let Some(value) = source_line(line) {
                source = Some(value);
                continue;
            }
            content.push(line.strip_prefix("  ").unwrap_or(line).to_string());
        }
    } else if markdown_time(first).is_some() {
        for line in &block.lines[1..] {
            if let Some(value) = source_line(line) {
                source = Some(value);
                continue;
            }
            content.push(line.clone());
        }
    } else {
        return None;
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
    let normalized = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    (!normalized.is_empty()).then_some(normalized)
}

/// 围栏的开标记：字符（` 或 ~）与长度。闭合标记必须同字符且不短于它。
#[derive(Clone, Copy)]
struct Fence {
    marker: char,
    len: usize,
}

fn entry_header(line: &str) -> Option<(String, String)> {
    if let Some(time) = markdown_time(line) {
        // Markdown 条目的正文从下一行开始，标题行本身不带内容。
        return Some((time, String::new()));
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
fn render_file(date: &str, header: &[String], blocks: &[RawEntry]) -> String {
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
        let text = match read_note(&path) {
            Ok(text) => text,
            Err(error) => {
                log::warn!("读取笔记文件失败 {}：{error}", path.display());
                continue;
            }
        };
        let mut parsed = parse_entries(&text);
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
    let needle = query.trim().to_lowercase();
    if needle.is_empty() || !dir.exists() {
        return Ok(Vec::new());
    }

    let mut result = Vec::new();
    for path in note_files(dir)? {
        let date = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("")
            .to_string();
        let text = match read_note(&path) {
            Ok(text) => text,
            Err(error) => {
                log::warn!("读取笔记文件失败 {}：{error}", path.display());
                continue;
            }
        };
        let mut parsed = parse_entries(&text);
        parsed.reverse();
        for (time, content, source) in parsed {
            let haystack = format!(
                "{date} {time} {content} {}",
                source.as_deref().unwrap_or("")
            )
            .to_lowercase();
            if haystack.contains(&needle) {
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
