use chrono::{DateTime, Local, NaiveDate};
use serde::Serialize;
use std::{
    fs::{self, OpenOptions},
    io::Write,
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

struct ParsedEntry {
    markdown: bool,
    time: String,
    content: Vec<String>,
    source: Option<String>,
}

impl ParsedEntry {
    fn finish(self) -> Option<(String, String, Option<String>)> {
        let content = self
            .content
            .join("\n")
            .trim_matches('\n')
            .trim_end()
            .to_string();
        if content.is_empty() {
            return None;
        }
        Some((self.time, content, self.source))
    }
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

fn fence_marker(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("```") || trimmed.starts_with("~~~")
}

/// 解析单个日期文件，兼容紧凑列表与 Markdown 两种格式。
pub fn parse_entries(text: &str) -> Vec<(String, String, Option<String>)> {
    let mut entries = Vec::new();
    let mut current: Option<ParsedEntry> = None;
    let mut in_fence = false;

    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');

        if in_fence {
            if let Some(entry) = current.as_mut() {
                let content_line = if entry.markdown {
                    line
                } else {
                    line.strip_prefix("  ").unwrap_or(line)
                };
                if fence_marker(content_line) {
                    in_fence = false;
                }
                entry.content.push(content_line.to_string());
            }
            continue;
        }

        if let Some(time) = markdown_time(line) {
            if let Some(done) = current.take().and_then(ParsedEntry::finish) {
                entries.push(done);
            }
            current = Some(ParsedEntry {
                markdown: true,
                time,
                content: Vec::new(),
                source: None,
            });
            continue;
        }

        if let Some((time, first)) = list_time(line) {
            if let Some(done) = current.take().and_then(ParsedEntry::finish) {
                entries.push(done);
            }
            current = Some(ParsedEntry {
                markdown: false,
                time,
                content: if first.is_empty() { Vec::new() } else { vec![first] },
                source: None,
            });
            continue;
        }

        let Some(entry) = current.as_mut() else {
            continue;
        };

        if let Some(source) = source_line(line) {
            entry.source = Some(source);
            continue;
        }

        let content_line = if entry.markdown {
            line
        } else {
            line.strip_prefix("  ").unwrap_or(line)
        };
        if fence_marker(content_line) {
            in_fence = true;
        }
        entry.content.push(content_line.to_string());
    }

    if let Some(done) = current.and_then(ParsedEntry::finish) {
        entries.push(done);
    }
    entries
}

/// 读取最近的笔记，按日期与写入顺序倒序返回。
pub fn load_recent(dir: &Path, limit: usize) -> std::io::Result<Vec<NoteEntry>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

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

    let mut result = Vec::new();
    for path in files {
        let date = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("")
            .to_string();
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let mut parsed = parse_entries(&text);
        parsed.reverse();
        for (time, content, source) in parsed {
            result.push(NoteEntry {
                date: date.clone(),
                time,
                content,
                source,
                path: path.to_string_lossy().to_string(),
            });
            if result.len() >= limit {
                return Ok(result);
            }
        }
    }
    Ok(result)
}

/// 追加到 <dir>/<date>.md。文件不存在时写入日期标题。
pub fn append_note(dir: &Path, date: &str, entry: &str) -> std::io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("{date}.md"));
    let existing = fs::read_to_string(&path).unwrap_or_default();
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;

    if existing.is_empty() {
        write!(file, "# {date}\n\n")?;
    } else if !existing.ends_with("\n\n") {
        write!(file, "\n")?;
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
        let text = "# 2026-10-21\n\n- 14:32 first\n  second\n  _来源：Chrome — 文章_\n\n- 15:02 next\n";
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
        assert_eq!(
            entries[0].1,
            "```\n### 11:00\n- 12:00 not an entry\n```"
        );
    }

    #[test]
    fn loads_recent_entries_newest_first() {
        let dir = std::env::temp_dir().join(format!("flashnote-test-recent-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);

        append_note(&dir, "2026-10-20", "- 10:00 older").unwrap();
        append_note(&dir, "2026-10-21", "- 09:00 newer").unwrap();

        let entries = load_recent(&dir, 10).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].date, "2026-10-21");
        assert_eq!(entries[0].time, "09:00");
        assert_eq!(entries[1].date, "2026-10-20");

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
