use chrono::{DateTime, Local};
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
